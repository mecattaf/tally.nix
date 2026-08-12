use super::text::compact_text;
use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::process::{Command as ProcessCommand, Stdio};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tally_core::adapters::{AdapterConfig, AdapterHardening};
use tally_core::campaign_contract::{
    admit_manifest_value, task_completion_revision, validate_argv, validate_manifest,
    CampaignAgent, CampaignGate, CampaignManifest, CanonicalCampaignGraphV1,
    CanonicalCampaignTaskV1, CAMPAIGN_SCHEMA_VERSION,
};
use tally_core::campaign_poll::{CampaignPollEvent, CampaignPollStatus};
use tally_core::campaign_registry::{
    CampaignRegistration, CampaignRegistrationV3, CampaignRegistry, REGISTRY_SCHEMA_VERSION,
};
use tally_core::config::{PoolConfig, ResourceKind};
use tally_core::lease::{is_campaign_pool_name, CAMPAIGN_POOL_PREFIX};

const CAMPAIGN_BEGIN: &str = "<!-- tally:campaign:v1 -->";
const CAMPAIGN_END: &str = "<!-- tally:campaign:v1:end -->";
const WORKLIST_BEGIN: &str = "<!-- tally:campaign-worklist:v1 -->";
const WORKLIST_END: &str = "<!-- tally:campaign-worklist:v1:end -->";
const TASK_MARKER_PREFIX: &str = "<!-- tally:campaign-task:v1 id=";
const SYSTEM_COMMENT_PREFIX: &str = "<!-- tally:spec-build:";
const CAMPAIGN_COMPLETE_COMMENT_PREFIX: &str = "<!-- tally:campaign-complete:";
const CAMPAIGN_SUMMARY_COMMENT_PREFIX: &str = "<!-- tally:campaign-summary:";
const APPROVED_GRAPH_SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const MAX_APPROVED_GRAPH_SNAPSHOT_BYTES: u64 = 32 * 1024 * 1024;
const CAMPAIGN_PROJECTION_SCHEMA_VERSION: u32 = 1;
const MAX_CAMPAIGN_PROJECTION_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
struct ProjectTask {
    id: String,
    kind: String,
    title: String,
    body: String,
    issue: Option<u64>,
    dependencies: Vec<String>,
    conflict_domains: Option<Vec<String>>,
    argv: Option<Vec<String>>,
    runtime_max_sec: Option<u64>,
}

struct ProjectCheckpointBrief<'a> {
    campaign_name: &'a str,
    argv: &'a [String],
    runtime_max_sec: u64,
    dependencies: &'a [String],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubActor {
    login: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GithubIssue {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: Option<String>,
    state: String,
    html_url: String,
    updated_at: String,
    user: GithubActor,
    #[serde(default)]
    pull_request: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct GithubComment {
    id: u64,
    body: String,
    html_url: String,
    created_at: String,
    updated_at: String,
    user: GithubActor,
}

#[derive(Debug, Clone)]
struct IssueLocator {
    repository: String,
    number: u64,
    url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignProjectionV1 {
    schema_version: u32,
    code_repository: String,
    worklist_pattern: String,
    source_revision: String,
    worklist_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    issue: Option<ProjectedIssueV1>,
    #[serde(default)]
    sub_issue_walk: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProjectedIssueV1 {
    repository: String,
    number: u64,
    url: String,
}

impl CampaignProjectionV1 {
    fn locator(&self) -> Result<IssueLocator> {
        let issue = self.issue.as_ref().ok_or_else(|| {
            invalid("campaign has no forge issue projection; use the local worklist arm path")
        })?;
        let locator = parse_issue_url(&issue.url)?;
        if locator.repository != issue.repository || locator.number != issue.number {
            return Err(invalid(
                "campaign projection has inconsistent issue coordinates",
            ));
        }
        Ok(locator)
    }
}

#[derive(Debug, Clone)]
struct CampaignGraph {
    locator: IssueLocator,
    canonical: CanonicalCampaignGraphV1,
    master: GithubIssue,
    tasks: Vec<GithubIssue>,
}

/// The prior executable graph needed to interpret a later amendment.
///
/// This projection snapshot is generation-scoped beside authority, so
/// publishing arm N+1 cannot make an arm-N reader observe a graph that
/// disagrees with its authority digest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ApprovedGraphSnapshotV1 {
    schema_version: u32,
    registration_id: String,
    arm_serial: u64,
    graph: CanonicalCampaignGraphV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedAutoPardon {
    task_id: String,
    added_dependencies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct AutoPardonReceipt {
    task_id: String,
    added_dependencies: Vec<String>,
    resume_receipt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PardonScope {
    All,
    Tasks(BTreeSet<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PardonBoundary {
    id: u64,
    scope: PardonScope,
}

impl PardonBoundary {
    fn applies_to(&self, task_id: &str) -> bool {
        match &self.scope {
            PardonScope::All => true,
            PardonScope::Tasks(tasks) => tasks.contains(task_id),
        }
    }
}

#[derive(Debug)]
enum CampaignPollAttempt {
    Dispatched,
    Complete,
    Unchanged,
    RearmRequired {
        approved_graph_digest: String,
        live_graph_digest: String,
    },
}

pub(super) async fn run_campaign(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    command: CampaignCommand,
) -> Result<()> {
    match command {
        CampaignCommand::Arm(args) => {
            run_campaign_arm(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::Resume(args) => {
            run_campaign_resume(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::Project(args) => run_campaign_project(args),
        CampaignCommand::Poll(args) => {
            run_campaign_poll(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::Status(args) => {
            run_campaign_status(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::List(args) => run_campaign_list(args),
        CampaignCommand::Quiescent(args) => run_campaign_quiescent(args),
        CampaignCommand::Disarm(args) => run_campaign_disarm(args),
    }
}

fn parse_issue_url(value: &str) -> Result<IssueLocator> {
    let prefix = "https://github.com/";
    let remainder = value.strip_prefix(prefix).ok_or_else(|| {
        invalid("campaign issue must use an https://github.com/OWNER/REPO/issues/NUMBER URL")
    })?;
    if remainder.contains(['?', '#']) || remainder.ends_with('/') {
        return Err(invalid(
            "campaign issue URL must not contain a query, fragment, or trailing slash",
        ));
    }
    let parts = remainder.split('/').collect::<Vec<_>>();
    if parts.len() != 4
        || parts[2] != "issues"
        || !safe_repo_part(parts[0])
        || !safe_repo_part(parts[1])
    {
        return Err(invalid(
            "campaign issue must use an https://github.com/OWNER/REPO/issues/NUMBER URL",
        ));
    }
    let number = parts[3]
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| invalid("campaign issue number must be positive"))?;
    Ok(IssueLocator {
        repository: format!("{}/{}", parts[0], parts[1]),
        number,
        url: value.to_owned(),
    })
}

fn parse_repository(value: &str) -> Result<String> {
    let parts = value.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || !safe_repo_part(parts[0]) || !safe_repo_part(parts[1]) {
        return Err(invalid("--repo must use safe OWNER/REPO form"));
    }
    Ok(value.to_owned())
}

fn parse_worklist_pattern(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 1_024
        || value.starts_with('/')
        || value.ends_with('/')
        || value.contains('\0')
        || value
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(invalid(
            "campaign worklist must be a relative pattern without empty, '.' or '..' components",
        ));
    }
    Ok(value.to_owned())
}

fn campaign_identity(code_repository: &str, worklist_pattern: &str) -> Result<(String, String)> {
    Ok((
        parse_repository(code_repository)?,
        parse_worklist_pattern(worklist_pattern)?,
    ))
}

fn safe_repo_part(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value != "."
        && value != ".."
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

fn safe_task_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn gh_program() -> OsString {
    std::env::var_os("TALLY_GH_PROGRAM").unwrap_or_else(|| OsString::from("gh"))
}

/// Run one `gh` invocation and hand back its raw result.
///
/// Every ordinary caller wants `run_gh`, which turns a non-zero exit into an
/// error. The capability probe is the exception: it has to read what the forge
/// actually said before it decides whether the failure was an answer.
fn run_gh_output(arguments: &[OsString], stdin: Option<&str>) -> Result<std::process::Output> {
    let program = gh_program();
    let mut command = ProcessCommand::new(&program);
    command
        .args(arguments)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("cannot execute {}", PathBuf::from(&program).display()))?;
    if let Some(input) = stdin {
        use std::io::Write as _;
        child
            .stdin
            .take()
            .context("gh stdin was unavailable")?
            .write_all(input.as_bytes())?;
    }
    Ok(child.wait_with_output()?)
}

fn run_gh(arguments: &[OsString], stdin: Option<&str>) -> Result<String> {
    let output = run_gh_output(arguments, stdin)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        bail!(
            "gh {:?} exited {}: {}",
            arguments,
            output.status,
            if detail.is_empty() {
                "no output"
            } else {
                &detail
            }
        );
    }
    String::from_utf8(output.stdout).context("gh stdout was not valid UTF-8")
}

fn gh_json<T: for<'de> Deserialize<'de>>(arguments: &[OsString]) -> Result<T> {
    let output = run_gh(arguments, None)?;
    serde_json::from_str(&output)
        .with_context(|| format!("gh {:?} returned invalid JSON", arguments))
}

fn os_arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn safe_github_login(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 39
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn authenticated_actor() -> Result<String> {
    let actor: GithubActor = gh_json(&os_arguments(&["api", "user"]))?;
    if !safe_github_login(&actor.login) {
        return Err(invalid("gh api user returned an invalid GitHub login"));
    }
    Ok(actor.login.to_ascii_lowercase())
}

fn local_actor() -> String {
    // SAFETY: `geteuid` has no preconditions and does not mutate process state.
    format!("uid:{}", unsafe { libc::geteuid() })
}

fn normalize_allowed_actors(values: &[String], authenticated: &str) -> Result<Vec<String>> {
    let mut actors = if values.is_empty() {
        BTreeSet::from([authenticated.to_owned()])
    } else {
        values
            .iter()
            .map(|value| {
                if !safe_github_login(value) {
                    return Err(invalid(format!(
                        "campaign --allow-actor value {value:?} is not a valid GitHub login"
                    )));
                }
                Ok(value.to_ascii_lowercase())
            })
            .collect::<Result<BTreeSet<_>>>()?
    };
    actors.insert(authenticated.to_owned());
    Ok(actors.into_iter().collect())
}

fn require_local_actor(registration: &CampaignRegistration) -> Result<()> {
    let actor = local_actor();
    if actor != registration.local_actor {
        bail!(
            "armed campaign {}/{} was approved by local actor {:?}, but the current local actor is {:?}; run the verb as the arming operator",
            registration.code_repository,
            registration.worklist_pattern,
            registration.local_actor,
            actor
        );
    }
    Ok(())
}

fn require_allowed_issue_authors(graph: &CampaignGraph, allowed: &[String]) -> Result<()> {
    let allowed = allowed.iter().map(String::as_str).collect::<BTreeSet<_>>();
    for (context, issue) in std::iter::once(("master", &graph.master))
        .chain(graph.tasks.iter().map(|issue| ("task", issue)))
    {
        let actor = issue.user.login.to_ascii_lowercase();
        if !allowed.contains(actor.as_str()) {
            bail!(
                "campaign {context} issue #{} was authored by unapproved actor {:?}; arm with --allow-actor only after reviewing that actor's input",
                issue.number,
                issue.user.login
            );
        }
    }
    Ok(())
}

fn fetch_issue_comments(locator: &IssueLocator) -> Result<Vec<GithubComment>> {
    let endpoint = format!(
        "repos/{}/issues/{}/comments?per_page=100",
        locator.repository, locator.number
    );
    let pages: Vec<Vec<GithubComment>> =
        gh_json(&os_arguments(&["api", "--paginate", "--slurp", &endpoint]))?;
    Ok(pages.into_iter().flatten().collect())
}

/// Comments written by the campaign mechanism, never operator steering.
///
/// Completion summaries use a campaign-level marker rather than the older
/// `spec-build` namespace. Treating their timestamp bump as fresh steering is
/// what let a poll queue one last pass while the same reconcile was closing
/// the master.
fn tally_authored_comment(body: &str) -> bool {
    [
        SYSTEM_COMMENT_PREFIX,
        CAMPAIGN_COMPLETE_COMMENT_PREFIX,
        CAMPAIGN_SUMMARY_COMMENT_PREFIX,
    ]
    .iter()
    .any(|marker| body.contains(marker))
}

fn fetch_steering(locator: &IssueLocator, allowed: &[String]) -> Result<Vec<Value>> {
    let allowed = allowed.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut comments = Vec::new();
    for comment in fetch_issue_comments(locator)? {
        if tally_authored_comment(&comment.body) {
            continue;
        }
        let actor = comment.user.login.to_ascii_lowercase();
        if !allowed.contains(actor.as_str()) {
            continue;
        }
        if comment.body.contains('\0') || comment.body.chars().count() > 64_000 {
            bail!(
                "approved steering comment {} exceeds the campaign comment contract",
                comment.html_url
            );
        }
        comments.push(json!({
            "id": comment.id,
            "url": comment.html_url,
            "author": actor,
            "body": comment.body,
            "createdAt": comment.created_at,
            "updatedAt": comment.updated_at,
        }));
    }
    if comments.len() > 1_000 {
        return Err(invalid(
            "campaign has more than 1000 approved steering comments",
        ));
    }
    Ok(comments)
}

fn fetch_issue(locator: &IssueLocator) -> Result<GithubIssue> {
    let endpoint = format!("repos/{}/issues/{}", locator.repository, locator.number);
    let issue: GithubIssue = gh_json(&os_arguments(&["api", &endpoint]))?;
    if issue.pull_request.is_some() {
        return Err(invalid(
            "campaign master URL names a pull request, not an issue",
        ));
    }
    if issue.number != locator.number || issue.html_url != locator.url {
        bail!("GitHub returned a different issue than the requested campaign locator");
    }
    Ok(issue)
}

fn fetch_subissues(locator: &IssueLocator) -> Result<Vec<GithubIssue>> {
    let endpoint = format!(
        "repos/{}/issues/{}/sub_issues?per_page=100",
        locator.repository, locator.number
    );
    gh_json(&os_arguments(&["api", &endpoint]))
}

/// The sub-issue surface the reconciler's read path and steering threads need.
///
/// One bounded GraphQL query walks the parent's sub-issues, the pull requests
/// that close each of them, and each thread's comments. `arm` runs it once as
/// a capability probe; every pass afterwards runs it for real.
const SUB_ISSUE_THREAD_QUERY: &str = r"
query($owner: String!, $name: String!, $number: Int!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      subIssues(first: 50, after: $cursor) {
        pageInfo { hasNextPage endCursor }
        nodes {
          number
          closedByPullRequestsReferences(first: 1, includeClosedPrs: true) {
            nodes { url merged }
          }
          comments(last: 100) {
            pageInfo { hasPreviousPage }
            nodes { databaseId url body createdAt updatedAt author { login } }
          }
        }
      }
    }
  }
}
";
/// The parent's sub-issue ceiling is 100 and a manifest caps at that number,
/// so two pages of 50 always cover an admitted graph.
const MAX_SUB_ISSUE_PAGES: usize = 2;
/// This is the window `SUB_ISSUE_THREAD_QUERY` asks for, and it is the steering
/// read: what survives it is what an agent is allowed to be steered by. `last:`
/// returns the newest, so an exhausted window drops the *oldest* comments —
/// `hasPreviousPage`, not `hasNextPage`. A thread long enough to exhaust it is
/// ordinary human discussion, which must not halt a campaign, so the truncation
/// is reported rather than refused.
const MAX_SUB_ISSUE_COMMENTS: usize = 100;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GraphqlComment {
    #[serde(default)]
    database_id: Option<u64>,
    url: String,
    body: String,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    author: Option<GithubActor>,
}

fn sub_issue_walk_arguments(
    owner: &str,
    name: &str,
    number: u64,
    cursor: Option<&str>,
) -> Vec<OsString> {
    let mut arguments = os_arguments(&["api", "graphql", "-f"]);
    arguments.push(format!("query={SUB_ISSUE_THREAD_QUERY}").into());
    arguments.extend(os_arguments(&["-F"]));
    arguments.push(format!("owner={owner}").into());
    arguments.extend(os_arguments(&["-F"]));
    arguments.push(format!("name={name}").into());
    arguments.extend(os_arguments(&["-F"]));
    arguments.push(format!("number={number}").into());
    if let Some(cursor) = cursor {
        arguments.extend(os_arguments(&["-F"]));
        arguments.push(format!("cursor={cursor}").into());
    }
    arguments
}

/// One bounded walk of the parent's sub-issue threads, plus the numbers whose
/// comment window the forge truncated.
#[derive(Debug, Clone, Default)]
struct SubIssueThreads {
    threads: BTreeMap<u64, Vec<GraphqlComment>>,
    /// Sub-issues carrying more than `MAX_SUB_ISSUE_COMMENTS` comments. The
    /// oldest fell out of the window this walk read, so an approved steering
    /// comment on one of them can no longer reach its task.
    truncated: BTreeSet<u64>,
}

fn sub_issue_threads(locator: &IssueLocator) -> Result<SubIssueThreads> {
    let (owner, name) = locator
        .repository
        .split_once('/')
        .ok_or_else(|| invalid("campaign repository is not in owner/name form"))?;
    let mut walked = SubIssueThreads::default();
    let threads = &mut walked.threads;
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_SUB_ISSUE_PAGES {
        let arguments = sub_issue_walk_arguments(owner, name, locator.number, cursor.as_deref());
        let payload: Value = gh_json(&arguments)?;
        let connection = payload
            .pointer("/data/repository/issue/subIssues")
            .ok_or_else(|| invalid("campaign sub-issue walk returned no sub-issue connection"))?;
        let nodes = connection
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("campaign sub-issue walk returned a malformed node list"))?;
        for node in nodes {
            let number = node
                .get("number")
                .and_then(Value::as_u64)
                .filter(|number| *number > 0)
                .ok_or_else(|| invalid("campaign sub-issue walk returned an invalid number"))?;
            if node.pointer("/comments/pageInfo/hasPreviousPage") == Some(&Value::Bool(true)) {
                walked.truncated.insert(number);
            }
            let comments = node
                .pointer("/comments/nodes")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new()));
            let comments: Vec<GraphqlComment> = serde_json::from_value(comments)
                .map_err(|error| invalid(format!("campaign sub-issue #{number}: {error}")))?;
            if threads.insert(number, comments).is_some() {
                bail!("campaign sub-issue walk repeated sub-issue #{number}");
            }
        }
        if connection.pointer("/pageInfo/hasNextPage") != Some(&Value::Bool(true)) {
            return Ok(walked);
        }
        cursor = Some(
            connection
                .pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("campaign sub-issue walk returned no page cursor"))?
                .to_owned(),
        );
    }
    Err(invalid(
        "campaign parent carries more sub-issues than the 100-task cap admits",
    ))
}

/// Did the forge answer "my schema has no such field" in a typed `errors[]`
/// entry?
///
/// This reads the response's own top-level `errors` array and nothing else.
/// GitHub types a schema refusal `UNDEFINED_FIELD`; older responses spell the
/// same thing as `extensions.code = "undefinedField"`.
fn typed_undefined_field(payload: &Value) -> bool {
    payload
        .get("errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| {
            errors.iter().any(|error| {
                error
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("UNDEFINED_FIELD"))
                    || error
                        .pointer("/extensions/code")
                        .and_then(Value::as_str)
                        .is_some_and(|code| code.eq_ignore_ascii_case("undefinedField"))
            })
        })
}

/// Does one line of forge-authored *error* text name a missing schema field?
///
/// Only ever applied to `errors[].message` and to `gh`'s own stderr, and only
/// on a call that actually failed. It must never see a response body: the walk
/// payload carries `comments { nodes { body } }`, and a comment body on a
/// public repository is writable by any account — including, through the
/// machine diagnosis receipts tally posts to task threads, by the campaign's
/// own agents. Scanning the whole payload made a stranger quoting an ordinary
/// GraphQL error (or quoting issue #334, which contains the literal string
/// `UNDEFINED_FIELD`) enough to answer the capability gate, and the gate fails
/// *open into degraded mode*: a campaign armed with no per-task steering
/// threads and no merged-oracle walk for the life of that arm.
fn undefined_field_message(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.contains("undefined_field")
        || lowered.contains("undefinedfield")
        || lowered.contains("doesn't exist on type")
        || lowered.contains("does not exist on type")
}

/// The `message` of every entry in a GraphQL response's `errors` array.
fn graphql_error_messages(payload: &Value) -> impl Iterator<Item = &str> {
    payload
        .get("errors")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|error| error.get("message").and_then(Value::as_str))
}

/// Probe whether this forge serves the sub-issue walk the native read path
/// needs.
///
/// A schema refusal is a capability answer, not a campaign failure: the
/// campaign arms in degraded mode and keeps the checkbox projection. Anything
/// else fails the arm. Treating every failure as an answer meant one flaky
/// round trip could arm a campaign with no per-task steering threads, no
/// merged-oracle walk and no anomaly surface, for the rest of its life, and
/// the only evidence would be the projection label.
fn probe_sub_issue_walk(locator: &IssueLocator) -> Result<bool> {
    let (owner, name) = locator
        .repository
        .split_once('/')
        .ok_or_else(|| invalid("campaign repository is not in owner/name form"))?;
    let arguments = sub_issue_walk_arguments(owner, name, locator.number, None);
    let output = run_gh_output(&arguments, None)?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let payload = serde_json::from_str::<Value>(&stdout).ok();
    // The typed answer is safe to read whatever the exit status was: it looks
    // at the response's own `errors` array, which no comment body can occupy.
    if payload.as_ref().is_some_and(typed_undefined_field) {
        return Ok(false);
    }
    if !output.status.success() {
        // Only now, and only over text the forge produced as an error.
        let refused = payload
            .as_ref()
            .is_some_and(|payload| graphql_error_messages(payload).any(undefined_field_message))
            || undefined_field_message(&stderr);
        if refused {
            return Ok(false);
        }
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        bail!(
            "campaign sub-issue capability probe failed: gh exited {}: {}; \
             this is not a capability answer, so the campaign is not armed degraded",
            output.status,
            if detail.is_empty() {
                "no output"
            } else {
                detail
            }
        );
    }
    // A call that succeeded and carries no typed refusal served the walk. What
    // its payload happens to *say* is not a capability answer.
    let payload = payload.context("campaign sub-issue capability probe returned invalid JSON")?;
    payload
        .pointer("/data/repository/issue/subIssues")
        .ok_or_else(|| {
            invalid("campaign sub-issue capability probe returned no sub-issue connection")
        })?;
    Ok(true)
}

const fn projection_label(sub_issue_walk: bool) -> &'static str {
    if sub_issue_walk {
        "native-sub-issues"
    } else {
        "degraded-checkboxes"
    }
}

/// Say which projection the campaign just armed with, on every arm path.
///
/// A campaign that armed degraded behaves differently in ways an operator
/// cannot see from the forge: no per-task steering threads, no merged-oracle
/// walk, no anomalies, checkbox writes back on the master. Reporting it only
/// on the `--no-enqueue` branch made the ordinary path silent about the one
/// fact that explains all of that.
fn armed_projection(result: &Value, sub_issue_walk: bool) -> Value {
    let mut value = result.clone();
    match value.as_object_mut() {
        Some(object) => {
            object.insert("subIssueWalk".to_owned(), json!(sub_issue_walk));
            object.insert(
                "projection".to_owned(),
                json!(projection_label(sub_issue_walk)),
            );
            value
        }
        None => json!({
            "result": value,
            "subIssueWalk": sub_issue_walk,
            "projection": projection_label(sub_issue_walk),
        }),
    }
}

fn arm_receipt(
    result: &Value,
    sub_issue_walk: bool,
    auto_pardons: &[AutoPardonReceipt],
    warnings: &[String],
) -> Value {
    let mut value = armed_projection(result, sub_issue_walk);
    let object = value
        .as_object_mut()
        .expect("armed_projection always returns an object");
    object.insert("autoPardons".to_owned(), json!(auto_pardons));
    let mut combined = object
        .remove("warnings")
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    combined.extend(warnings.iter().map(|warning| json!(warning)));
    object.insert("warnings".to_owned(), Value::Array(combined));
    value
}

/// Every steering surface a pass reads: the campaign-wide master thread, plus
/// each task's own sub-issue thread where the forge serves one.
#[derive(Debug, Clone, Default)]
struct CampaignSteering {
    master: Vec<Value>,
    tasks: BTreeMap<String, Vec<Value>>,
}

fn fetch_campaign_steering(
    graph: &CampaignGraph,
    allowed: &[String],
    native: bool,
) -> Result<CampaignSteering> {
    let master = fetch_steering(&graph.locator, allowed)?;
    let tasks = if native {
        let numbers = graph
            .tasks
            .iter()
            .map(|issue| issue.number)
            .collect::<Vec<_>>();
        task_steering(&graph.locator, allowed, &numbers)?
    } else {
        BTreeMap::new()
    };
    Ok(CampaignSteering { master, tasks })
}

fn task_steering(
    locator: &IssueLocator,
    allowed: &[String],
    subissues: &[u64],
) -> Result<BTreeMap<String, Vec<Value>>> {
    let walked = sub_issue_threads(locator)?;
    // Reported, never refused. The window is exhausted by ordinary human
    // discussion, and halting a campaign over that would be worse than the
    // truncation; but a steering comment that scrolled out of it silently
    // stops reaching its agent, and nothing else on this path would say so.
    for number in subissues {
        if walked.truncated.contains(number) {
            errln!(
                "tally: campaign sub-issue #{number} carries more than \
                 {MAX_SUB_ISSUE_COMMENTS} comments; the steering read sees only the newest \
                 {MAX_SUB_ISSUE_COMMENTS} and an approved comment older than that can no \
                 longer reach this task"
            );
        }
    }
    let threads = walked.threads;
    let allowed = allowed.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut steering = BTreeMap::new();
    for number in subissues {
        let mut comments = Vec::new();
        for comment in threads.get(number).into_iter().flatten() {
            if tally_authored_comment(&comment.body) {
                continue;
            }
            let Some(author) = comment.author.as_ref() else {
                continue;
            };
            let actor = author.login.to_ascii_lowercase();
            if !allowed.contains(actor.as_str()) {
                continue;
            }
            if comment.body.contains('\0') || comment.body.chars().count() > 64_000 {
                bail!(
                    "approved steering comment {} exceeds the campaign comment contract",
                    comment.url
                );
            }
            let Some(id) = comment.database_id else {
                continue;
            };
            comments.push(json!({
                "id": id,
                "url": comment.url,
                "author": actor,
                "body": comment.body,
                "createdAt": comment.created_at,
                "updatedAt": comment.updated_at,
            }));
        }
        if comments.len() > 1_000 {
            return Err(invalid(format!(
                "campaign sub-issue #{number} has more than 1000 approved steering comments"
            )));
        }
        if !comments.is_empty() {
            steering.insert(number.to_string(), comments);
        }
    }
    Ok(steering)
}

fn machine_marker_fields<'a>(body: &'a str, kind: &str) -> Option<BTreeMap<&'a str, &'a str>> {
    let prefix = format!("<!-- tally:spec-build:{kind}:v1 ");
    let content = body
        .lines()
        .next()?
        .strip_prefix(&prefix)?
        .strip_suffix(" -->")?;
    let mut fields = BTreeMap::new();
    for token in content.split(' ') {
        let (name, value) = token.split_once('=')?;
        if name.is_empty() || value.is_empty() || fields.insert(name, value).is_some() {
            return None;
        }
    }
    Some(fields)
}

fn diagnosis_marker_task(body: &str, campaign: &str, issue_number: u64) -> Option<(String, u8)> {
    let fields = machine_marker_fields(body, "diagnosis")?;
    if fields.len() != 4
        || fields.get("campaign").copied() != Some(campaign)
        || fields
            .get("issue")
            .and_then(|value| value.parse::<u64>().ok())
            != Some(issue_number)
    {
        return None;
    }
    let task_id = fields.get("task").copied()?;
    let attempt = fields.get("attempt")?.parse::<u8>().ok()?;
    (safe_task_id(task_id) && matches!(attempt, 1 | 2)).then(|| (task_id.to_owned(), attempt))
}

fn escalation_comment(body: &str, campaign: &str, issue_number: u64) -> bool {
    body.lines().next()
        == Some(
            format!(
                "<!-- tally:spec-build:escalation:v1 campaign={campaign} issue={issue_number} -->"
            )
            .as_str(),
        )
}

fn resume_marker_scope(
    body: &str,
    campaign: &str,
    issue_number: u64,
) -> Result<Option<PardonScope>> {
    let Some(fields) = machine_marker_fields(body, "resume") else {
        return Ok(None);
    };
    if !matches!(fields.len(), 3 | 4)
        || fields
            .keys()
            .any(|field| !matches!(*field, "campaign" | "issue" | "nonce" | "tasks"))
        || fields.get("campaign").copied() != Some(campaign)
        || fields
            .get("issue")
            .and_then(|value| value.parse::<u64>().ok())
            != Some(issue_number)
    {
        return Ok(None);
    }
    let Some(nonce) = fields.get("nonce") else {
        return Ok(None);
    };
    if uuid::Uuid::parse_str(nonce).is_err() {
        return Err(invalid("campaign resume marker carries an invalid nonce"));
    }
    if !body.contains("\n\n### Campaign resumed\n\n") || !body.contains("\n\nReason: ") {
        return Err(invalid("campaign resume receipt has malformed content"));
    }
    let Some(tasks) = fields.get("tasks") else {
        return Ok(Some(PardonScope::All));
    };
    let tasks = tasks.split(',').map(str::to_owned).collect::<Vec<_>>();
    let unique = tasks.iter().cloned().collect::<BTreeSet<_>>();
    if tasks.is_empty()
        || tasks.len() != unique.len()
        || tasks.iter().any(|task_id| !safe_task_id(task_id))
    {
        return Err(invalid(
            "campaign resume receipt carries invalid scoped tasks",
        ));
    }
    Ok(Some(PardonScope::Tasks(unique)))
}

fn active_escalated_tasks(
    graph: &CampaignGraph,
    authenticated_actor: &str,
    sub_issue_walk: bool,
) -> Result<BTreeSet<String>> {
    let campaign = &graph.canonical.manifest.name;
    let issue_number = graph.locator.number;
    let mut boundaries = Vec::new();
    let mut diagnoses: BTreeMap<String, Vec<(u64, u8)>> = BTreeMap::new();
    let mut escalations = Vec::new();

    let master_comments = fetch_issue_comments(&graph.locator)?;
    for comment in &master_comments {
        if !comment.user.login.eq_ignore_ascii_case(authenticated_actor) {
            continue;
        }
        if let Some(scope) = resume_marker_scope(&comment.body, campaign, issue_number)? {
            boundaries.push(PardonBoundary {
                id: comment.id,
                scope,
            });
        }
        if let Some((task_id, attempt)) =
            diagnosis_marker_task(&comment.body, campaign, issue_number)
        {
            diagnoses
                .entry(task_id)
                .or_default()
                .push((comment.id, attempt));
        }
        if escalation_comment(&comment.body, campaign, issue_number) {
            escalations.push(comment.id);
        }
    }

    if sub_issue_walk {
        let task_by_issue = graph
            .canonical
            .manifest
            .tasks
            .iter()
            .map(|task| (task.issue, task.id.as_str()))
            .collect::<BTreeMap<_, _>>();
        let walked = sub_issue_threads(&graph.locator)?;
        for (issue, comments) in walked.threads {
            let Some(expected_task) = task_by_issue.get(&issue) else {
                continue;
            };
            for comment in comments {
                let Some(author) = comment.author.as_ref() else {
                    continue;
                };
                if !author.login.eq_ignore_ascii_case(authenticated_actor) {
                    continue;
                }
                let Some((task_id, attempt)) =
                    diagnosis_marker_task(&comment.body, campaign, issue_number)
                else {
                    continue;
                };
                if task_id != *expected_task {
                    continue;
                }
                if let Some(id) = comment.database_id {
                    diagnoses.entry(task_id).or_default().push((id, attempt));
                }
            }
        }
    }

    let current_tasks = graph
        .canonical
        .manifest
        .tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut active = BTreeSet::new();
    for (task_id, receipts) in diagnoses {
        if !current_tasks.contains(task_id.as_str()) {
            continue;
        }
        let boundary = boundaries
            .iter()
            .filter(|candidate| candidate.applies_to(&task_id))
            .map(|candidate| candidate.id)
            .max()
            .unwrap_or(0);
        let current = receipts
            .into_iter()
            .filter(|(id, _)| *id > boundary)
            .collect::<Vec<_>>();
        let attempts = current
            .iter()
            .map(|(_, attempt)| *attempt)
            .collect::<BTreeSet<_>>();
        let last_diagnosis = current.iter().map(|(id, _)| *id).max().unwrap_or(0);
        if attempts == BTreeSet::from([1, 2])
            && escalations
                .iter()
                .any(|escalation| *escalation > boundary && *escalation > last_diagnosis)
        {
            active.insert(task_id);
        }
    }
    Ok(active)
}

fn amendment_pardon_plan(
    prior: Option<&CanonicalCampaignGraphV1>,
    current: &CanonicalCampaignGraphV1,
    escalated: &BTreeSet<String>,
) -> (Vec<PlannedAutoPardon>, Vec<String>) {
    let prior_dependencies = prior
        .map(|graph| {
            graph
                .manifest
                .tasks
                .iter()
                .map(|task| {
                    (
                        task.id.as_str(),
                        task.dependencies
                            .iter()
                            .map(String::as_str)
                            .collect::<BTreeSet<_>>(),
                    )
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();
    let mut pardons = Vec::new();
    let mut addressed = BTreeSet::new();
    for task in &current.manifest.tasks {
        if !escalated.contains(&task.id) {
            continue;
        }
        let Some(previous) = prior_dependencies.get(task.id.as_str()) else {
            continue;
        };
        let added_dependencies = task
            .dependencies
            .iter()
            .filter(|dependency| !previous.contains(dependency.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        if !added_dependencies.is_empty() {
            addressed.insert(task.id.clone());
            pardons.push(PlannedAutoPardon {
                task_id: task.id.clone(),
                added_dependencies,
            });
        }
    }
    let warnings = escalated
        .difference(&addressed)
        .map(|task_id| {
            format!("task {task_id} remains escalated; run tally campaign resume to unblock")
        })
        .collect();
    (pardons, warnings)
}

fn extract_managed_section<'a>(body: &'a str, start: &str, end: &str) -> Result<&'a str> {
    let start_index = body
        .find(start)
        .ok_or_else(|| invalid(format!("campaign issue body is missing {start}")))?;
    let content_start = start_index + start.len();
    let remainder = &body[content_start..];
    let relative_end = remainder
        .find(end)
        .ok_or_else(|| invalid(format!("campaign issue body is missing {end}")))?;
    let tail = &remainder[relative_end + end.len()..];
    if tail.contains(start) || tail.contains(end) {
        return Err(invalid(format!(
            "campaign issue body repeats a {start}/{end} marker"
        )));
    }
    Ok(remainder[..relative_end].trim())
}

fn parse_manifest(body: &str, repository: &str) -> Result<CampaignManifest> {
    let section = extract_managed_section(body, CAMPAIGN_BEGIN, CAMPAIGN_END)?;
    let json = section
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .ok_or_else(|| invalid("campaign manifest must be one fenced JSON object"))?;
    let mut value: Value = serde_json::from_str(json)
        .map_err(|error| invalid(format!("campaign manifest is invalid: {error}")))?;
    if let Some(manifest) = value.as_object_mut() {
        let repository_pool = format!("{CAMPAIGN_POOL_PREFIX}{repository}");
        match manifest.get("pool") {
            None => {
                manifest.insert("pool".to_owned(), json!(repository_pool));
            }
            Some(Value::String(pool)) if pool.starts_with(CAMPAIGN_POOL_PREFIX) => {
                if !is_campaign_pool_name(pool) {
                    return Err(invalid(
                        "campaign namespace pool must use campaign/OWNER/REPO form",
                    ));
                }
                if pool != &repository_pool {
                    return Err(invalid(format!(
                        "campaign namespace pool must match issue repository {repository:?}"
                    )));
                }
            }
            Some(_) => {}
        }
    }
    Ok(admit_manifest_value(value)?)
}

fn fetch_campaign_graph(locator: &IssueLocator) -> Result<CampaignGraph> {
    let master = fetch_issue(locator)?;
    campaign_graph_from(locator, master)
}

/// Build the graph from a master issue the caller has already read.
///
/// The poll reads it first anyway, because a closed master prunes the
/// registration rather than failing the scan, and re-reading it here made every
/// idle tick pay for the same issue twice.
fn campaign_graph_from(locator: &IssueLocator, master: GithubIssue) -> Result<CampaignGraph> {
    if master.state != "open" {
        return Err(invalid("campaign master issue must be open"));
    }
    let manifest = parse_manifest(
        master.body.as_deref().unwrap_or_default(),
        &locator.repository,
    )?;
    validate_worklist_projection(master.body.as_deref().unwrap_or_default(), &manifest)?;
    let subissues = fetch_subissues(locator)?;
    let subissue_count = subissues.len();
    let by_number = subissues
        .into_iter()
        .map(|issue| (issue.number, issue))
        .collect::<BTreeMap<_, _>>();
    if by_number.len() != subissue_count {
        return Err(invalid(
            "GitHub returned duplicate native sub-issue numbers",
        ));
    }
    let expected = manifest
        .tasks
        .iter()
        .map(|task| task.issue)
        .collect::<BTreeSet<_>>();
    let actual = by_number.keys().copied().collect::<BTreeSet<_>>();
    if expected != actual {
        bail!(
            "campaign manifest task issues and GitHub native sub-issues differ (manifest: {:?}; sub-issues: {:?})",
            expected,
            actual
        );
    }
    let mut tasks = Vec::with_capacity(manifest.tasks.len());
    for reference in &manifest.tasks {
        let issue = by_number
            .get(&reference.issue)
            .expect("sets were compared above")
            .clone();
        if issue.pull_request.is_some() {
            return Err(invalid(format!(
                "campaign task {} names pull request #{}",
                reference.id, reference.issue
            )));
        }
        if issue.state != "open" && issue.state != "closed" {
            bail!("campaign task issue #{} has unknown state", issue.number);
        }
        let expected_url = format!(
            "https://github.com/{}/issues/{}",
            locator.repository, reference.issue
        );
        if issue.html_url != expected_url {
            bail!("campaign task {} issue URL is not canonical", reference.id);
        }
        if issue.title.trim().is_empty()
            || issue.title.chars().count() > 300
            || issue.title.chars().any(char::is_control)
        {
            return Err(invalid(format!(
                "campaign task {} issue #{} has an invalid title",
                reference.id, reference.issue
            )));
        }
        let body = issue.body.as_deref().unwrap_or_default();
        if body.trim().is_empty() || body.contains('\0') || body.chars().count() > 64_000 {
            return Err(invalid(format!(
                "campaign task {} issue #{} brief must contain 1..=64000 characters without NUL bytes",
                reference.id, reference.issue
            )));
        }
        tasks.push(issue);
    }
    // Managed checkbox state, issue open/closed projection, update timestamps,
    // and master prose outside the fenced manifest are deliberately excluded.
    // This canonical value is both what Rust hashes and what the flow carries.
    let canonical = CanonicalCampaignGraphV1::new(
        manifest,
        tasks
            .iter()
            .map(|issue| CanonicalCampaignTaskV1 {
                number: issue.number,
                title: issue.title.clone(),
                body: issue.body.as_deref().unwrap_or_default().to_owned(),
            })
            .collect(),
    )?;
    Ok(CampaignGraph {
        locator: locator.clone(),
        canonical,
        master,
        tasks,
    })
}

fn sha256_json(value: &Value) -> Result<String> {
    let canonical = tally_core::campaign_contract::canonical_json(value)?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes())))
}

fn forge_state_value(graph: &CampaignGraph) -> Value {
    json!({
        "master": {
            "state": graph.master.state,
            "updatedAt": graph.master.updated_at,
        },
        "tasks": graph.tasks.iter().map(|issue| json!({
            "number": issue.number,
            "state": issue.state,
            "updatedAt": issue.updated_at,
        })).collect::<Vec<_>>(),
    })
}

/// Forge facts with scheduling meaning after the steering read has completed.
///
/// `updated_at` belongs in the cheap precondition above because it tells the
/// poll when comments may need rereading. It does not belong in the admitted
/// observation itself: Tally's own comments and checkbox projection bump that
/// timestamp, while the filtered steering and executable graph stay exactly
/// the same.
fn campaign_state_value(graph: &CampaignGraph) -> Value {
    json!({
        "master": {
            "state": graph.master.state,
        },
        "tasks": graph.tasks.iter().map(|issue| json!({
            "number": issue.number,
            "state": issue.state,
        })).collect::<Vec<_>>(),
    })
}

fn campaign_state_ref_prefix(campaign: &str, issue_number: u64) -> String {
    let scope = format!("{campaign}\0{issue_number}");
    let digest = format!("{:x}", Sha256::digest(scope.as_bytes()));
    format!("refs/tally/spec-build/v1/{}", &digest[..24])
}

/// Durable Git state that can advance without touching an issue or comment.
///
/// The driver treats the remote base plus its campaign-scoped hidden refs as
/// the source of truth for local merges, checkpoints, and continuation
/// receipts. Public polls must read the same facts: otherwise a completed
/// local pass is indistinguishable from an idle campaign when the forge issue
/// itself did not move.
fn repository_progress_value(graph: &CampaignGraph) -> Result<Value> {
    let repository = &graph.canonical.manifest.repository;
    let base_ref = format!("refs/heads/{}", repository.base_branch);
    let state_prefix =
        campaign_state_ref_prefix(&graph.canonical.manifest.name, graph.locator.number);
    let state_pattern = format!("{state_prefix}/*");
    let listed = ProcessCommand::new("git")
        .arg("-C")
        .arg(&repository.checkout)
        .args([
            "ls-remote",
            "--refs",
            repository.remote.as_str(),
            base_ref.as_str(),
            state_pattern.as_str(),
        ])
        .output()
        .context("cannot query durable campaign repository state")?;
    if !listed.status.success() {
        bail!(
            "cannot query durable campaign repository state: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        );
    }
    let stdout =
        String::from_utf8(listed.stdout).context("git ls-remote output was not valid UTF-8")?;
    let mut base = None;
    let mut campaign_refs = BTreeMap::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let (target, name) = line
            .split_once('\t')
            .ok_or_else(|| invalid("campaign repository state contained a malformed ref"))?;
        if !((40..=64).contains(&target.len())
            && target.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(invalid(format!(
                "campaign repository ref {name} returned a malformed object ID"
            )));
        }
        if name == base_ref {
            if base.replace(target.to_owned()).is_some() {
                bail!("campaign repository returned the base ref more than once");
            }
        } else if name
            .strip_prefix(&state_prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
            && campaign_refs
                .insert(name.to_owned(), target.to_owned())
                .is_some()
        {
            bail!("campaign repository returned state ref {name} more than once");
        }
    }
    let base = base.ok_or_else(|| {
        invalid(format!(
            "campaign repository remote has no base ref {base_ref}"
        ))
    })?;
    Ok(json!({
        "base": {
            "ref": base_ref,
            "target": base,
        },
        "campaignRefs": campaign_refs,
    }))
}

/// The cheap external-state half of the full campaign observation.
///
/// The two REST reads cover every forge surface the expensive GraphQL steering
/// walk can reveal through issue `updated_at` or `state`. The one bounded
/// `ls-remote` covers driver progress that moves only Git. If both are stable,
/// the full observation is stable and the poll can skip the walk; if either
/// moves, the full revision and enqueue identity include the same change.
#[cfg(test)]
fn forge_observation(
    graph: &CampaignGraph,
    repository_progress: &Value,
    arm_serial: u64,
) -> Result<String> {
    sha256_json(&json!({
        "graph": graph.canonical.executable_digest,
        "forgeState": forge_state_value(graph),
        "repositoryProgress": repository_progress,
        "armSerial": arm_serial,
    }))
}

/// A bounded confirmation identity for an unapproved executable graph.
///
/// The first mismatch is retained here but is not yet called
/// `rearm-required`. A second identical registry scan confirms that the graph
/// is stable. This absorbs the one-read intermediate views GitHub can expose
/// while Tally is applying its own sequence of issue mutations, without ever
/// dispatching unapproved content.
#[cfg(test)]
fn graph_mismatch_observation(graph: &CampaignGraph, arm_serial: u64) -> Result<String> {
    sha256_json(&json!({
        "kind": "campaign-graph-mismatch-v1",
        "graph": graph.canonical.executable_digest,
        "forgeState": forge_state_value(graph),
        "armSerial": arm_serial,
    }))
}

fn graph_poll_snapshot(graph: &CampaignGraph) -> Result<String> {
    sha256_json(&json!({
        "graph": graph.canonical.executable_digest,
        "forgeState": forge_state_value(graph),
    }))
}

fn campaign_observation(
    graph: &CampaignGraph,
    steering: &CampaignSteering,
    repository_progress: &Value,
    arm_serial: u64,
) -> Result<String> {
    sha256_json(&json!({
        "graph": graph.canonical.executable_digest,
        "forgeState": campaign_state_value(graph),
        "repositoryProgress": repository_progress,
        "steering": steering.master,
        // A comment on one task's sub-issue thread must nudge the campaign
        // exactly like a comment on the master does.
        "taskSteering": steering.tasks,
        "armSerial": arm_serial,
    }))
}

fn resolve_state_dir(value: Option<PathBuf>) -> Result<PathBuf> {
    let path = value.map_or_else(default_state_dir, Ok)?;
    if !path.is_absolute() {
        return Err(invalid("campaign state directory must be absolute"));
    }
    Ok(path)
}

fn campaign_projection_path(
    state_dir: &Path,
    code_repository: &str,
    worklist_pattern: &str,
) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(code_repository.as_bytes());
    hasher.update([0]);
    hasher.update(worklist_pattern.as_bytes());
    state_dir
        .join("campaigns/projections")
        .join(format!("{:x}.projection-v1.json", hasher.finalize()))
}

fn read_campaign_projection(
    state_dir: &Path,
    code_repository: &str,
    worklist_pattern: &str,
) -> Result<CampaignProjectionV1> {
    let path = campaign_projection_path(state_dir, code_repository, worklist_pattern);
    let metadata = fs::metadata(&path).with_context(|| {
        format!(
            "campaign {code_repository}/{worklist_pattern} has no local forge projection at {}; run `tally campaign project` first when this campaign still uses a forge projection",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() > MAX_CAMPAIGN_PROJECTION_BYTES {
        bail!(
            "campaign projection {} is not a bounded regular file",
            path.display()
        );
    }
    let projection: CampaignProjectionV1 = serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("campaign projection {} is invalid", path.display()))?;
    if projection.schema_version != CAMPAIGN_PROJECTION_SCHEMA_VERSION
        || projection.code_repository != code_repository
        || projection.worklist_pattern != worklist_pattern
        || !projection
            .worklist_sha256
            .strip_prefix("sha256:")
            .is_some_and(|digest| {
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        || !matches!(projection.source_revision.len(), 40 | 64)
        || !projection
            .source_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        bail!(
            "campaign projection {} violates projection-v1 invariants",
            path.display()
        );
    }
    if projection.issue.is_some() {
        projection.locator()?;
    }
    Ok(projection)
}

fn write_campaign_projection(state_dir: &Path, projection: &CampaignProjectionV1) -> Result<()> {
    let path = campaign_projection_path(
        state_dir,
        &projection.code_repository,
        &projection.worklist_pattern,
    );
    let directory = path
        .parent()
        .expect("campaign projection path always has a parent");
    fs::create_dir_all(directory)?;
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::now_v7()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    serde_json::to_writer(&mut file, projection)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn committed_worklist_coordinate(
    worklist: &Path,
    manifest: &CampaignManifest,
) -> Result<(String, String, String)> {
    if worklist == Path::new("-") {
        return Err(invalid(
            "campaign project requires a committed worklist file; stdin has no repository identity",
        ));
    }
    let checkout = fs::canonicalize(&manifest.repository.checkout).with_context(|| {
        format!(
            "cannot resolve campaign checkout {}",
            manifest.repository.checkout.display()
        )
    })?;
    let worklist = fs::canonicalize(worklist)
        .with_context(|| format!("cannot resolve campaign worklist {}", worklist.display()))?;
    let relative = worklist.strip_prefix(&checkout).map_err(|_| {
        invalid(format!(
            "campaign worklist {} is outside code checkout {}",
            worklist.display(),
            checkout.display()
        ))
    })?;
    let pattern = relative
        .to_str()
        .ok_or_else(|| invalid("campaign worklist path is not valid UTF-8"))?
        .replace(std::path::MAIN_SEPARATOR, "/");
    let pattern = parse_worklist_pattern(&pattern)?;

    let git = |arguments: &[&str], context: &str| -> Result<std::process::Output> {
        ProcessCommand::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(arguments)
            .output()
            .with_context(|| format!("cannot execute git while {context}"))
    };
    let fetched = git(
        &["fetch", "--prune", "--no-tags", &manifest.repository.remote],
        "fetching the worklist authority revision",
    )?;
    if !fetched.status.success() {
        bail!(
            "cannot fetch campaign worklist authority revision: {}",
            String::from_utf8_lossy(&fetched.stderr).trim()
        );
    }
    let base_ref = format!(
        "{}/{}^{{commit}}",
        manifest.repository.remote, manifest.repository.base_branch
    );
    let resolved = git(
        &["rev-parse", "--verify", &base_ref],
        "resolving the worklist authority revision",
    )?;
    if !resolved.status.success() {
        bail!(
            "cannot resolve campaign worklist authority revision {base_ref}: {}",
            String::from_utf8_lossy(&resolved.stderr).trim()
        );
    }
    let revision = String::from_utf8(resolved.stdout)?
        .trim()
        .to_ascii_lowercase();
    let object = format!("{revision}:{pattern}");
    let committed = git(
        &["show", &object],
        "reading the committed campaign worklist",
    )?;
    if !committed.status.success() {
        bail!(
            "campaign worklist pattern {pattern:?} is not a regular file at fetched base revision {revision}"
        );
    }
    let local = fs::read(&worklist)?;
    if committed.stdout != local {
        bail!(
            "campaign worklist {} differs from {pattern:?} at fetched base revision {revision}; commit and push it before projecting",
            worklist.display()
        );
    }
    let digest = format!("sha256:{:x}", Sha256::digest(&committed.stdout));
    Ok((pattern, revision, digest))
}

fn approved_graph_directory(state_dir: &Path, registration_id: &str) -> PathBuf {
    let scope = format!("{:x}", Sha256::digest(registration_id.as_bytes()));
    state_dir
        .join("campaigns/approved-graphs")
        .join(&scope[..32])
}

fn approved_graph_path(state_dir: &Path, registration: &CampaignRegistration) -> PathBuf {
    approved_graph_directory(state_dir, &registration.registration_id)
        .join(format!("{}.graph-v1.json", registration.arm_serial))
}

fn validated_graph_snapshot(
    snapshot: ApprovedGraphSnapshotV1,
    registration: &CampaignRegistration,
    path: &Path,
) -> Result<CanonicalCampaignGraphV1> {
    if snapshot.schema_version != APPROVED_GRAPH_SNAPSHOT_SCHEMA_VERSION
        || snapshot.registration_id != registration.registration_id
        || snapshot.arm_serial != registration.arm_serial
        || snapshot.graph.executable_digest != registration.approved_graph_digest
    {
        bail!(
            "campaign approved-graph snapshot {} disagrees with registration {} arm {}",
            path.display(),
            registration.registration_id,
            registration.arm_serial
        );
    }
    let rebuilt = CanonicalCampaignGraphV1::new(
        snapshot.graph.manifest.clone(),
        snapshot.graph.tasks.clone(),
    )?;
    if rebuilt != snapshot.graph {
        bail!(
            "campaign approved-graph snapshot {} fails canonical digest verification",
            path.display()
        );
    }
    Ok(snapshot.graph)
}

fn read_approved_graph_snapshot(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<Option<CanonicalCampaignGraphV1>> {
    let path = approved_graph_path(state_dir, registration);
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("cannot inspect campaign approved graph {}", path.display())
            })
        }
    };
    if !metadata.is_file() || metadata.len() > MAX_APPROVED_GRAPH_SNAPSHOT_BYTES {
        bail!(
            "campaign approved-graph snapshot {} is not a bounded regular file",
            path.display()
        );
    }
    let bytes = fs::read(&path)
        .with_context(|| format!("cannot read campaign approved graph {}", path.display()))?;
    let snapshot: ApprovedGraphSnapshotV1 = serde_json::from_slice(&bytes)
        .with_context(|| format!("campaign approved graph {} is invalid", path.display()))?;
    validated_graph_snapshot(snapshot, registration, &path).map(Some)
}

fn write_approved_graph_snapshot(
    state_dir: &Path,
    registration: &CampaignRegistration,
    graph: &CanonicalCampaignGraphV1,
) -> Result<()> {
    if graph.executable_digest != registration.approved_graph_digest {
        bail!("cannot snapshot a campaign graph that disagrees with arm authority");
    }
    let directory = approved_graph_directory(state_dir, &registration.registration_id);
    fs::create_dir_all(&directory).with_context(|| {
        format!(
            "cannot create campaign approved-graph directory {}",
            directory.display()
        )
    })?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!(
            "cannot secure campaign approved-graph directory {}",
            directory.display()
        )
    })?;
    let path = approved_graph_path(state_dir, registration);
    let temporary = directory.join(format!(
        ".{}.{}.tmp",
        registration.arm_serial,
        uuid::Uuid::now_v7()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| {
            format!(
                "cannot create campaign approved-graph snapshot {}",
                temporary.display()
            )
        })?;
    let snapshot = ApprovedGraphSnapshotV1 {
        schema_version: APPROVED_GRAPH_SNAPSHOT_SCHEMA_VERSION,
        registration_id: registration.registration_id.clone(),
        arm_serial: registration.arm_serial,
        graph: graph.clone(),
    };
    serde_json::to_writer(&mut file, &snapshot)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, &path).with_context(|| {
        format!(
            "cannot publish campaign approved-graph snapshot {}",
            path.display()
        )
    })?;
    fs::File::open(&directory)?.sync_all()?;
    Ok(())
}

fn prune_approved_graph_snapshots(
    state_dir: &Path,
    registration: &CampaignRegistration,
) -> Result<()> {
    let directory = approved_graph_directory(state_dir, &registration.registration_id);
    let expected = approved_graph_path(state_dir, registration);
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path != expected && path.is_file() {
            fs::remove_file(&path).with_context(|| {
                format!(
                    "cannot prune obsolete campaign approved graph {}",
                    path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn remove_approved_graph_snapshots(state_dir: &Path, registration_id: &str) -> Result<()> {
    let directory = approved_graph_directory(state_dir, registration_id);
    match fs::remove_dir_all(&directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "cannot remove campaign approved graphs {}",
                directory.display()
            )
        }),
    }
}

fn default_campaign_workspace_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(path).join("tally/campaigns/workspaces"));
    }
    let home = std::env::var_os("HOME").context("HOME and XDG_CACHE_HOME are both unset")?;
    Ok(PathBuf::from(home).join(".cache/tally/campaigns/workspaces"))
}

fn resolve_asset(
    explicit: Option<PathBuf>,
    environment: &str,
    installed_relative: &str,
    development_relative: &str,
) -> Result<PathBuf> {
    let candidate = if let Some(path) = explicit {
        path
    } else if let Some(path) = std::env::var_os(environment) {
        PathBuf::from(path)
    } else {
        let installed = std::env::current_exe().ok().and_then(|executable| {
            executable
                .parent()
                .and_then(Path::parent)
                .map(|root| root.join(installed_relative))
        });
        installed.filter(|path| path.is_file()).unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(development_relative)
        })
    };
    let candidate = candidate
        .canonicalize()
        .with_context(|| format!("cannot resolve campaign asset {}", candidate.display()))?;
    if !candidate.is_file() {
        bail!(
            "campaign asset is not a regular file: {}",
            candidate.display()
        );
    }
    Ok(candidate)
}

fn resolve_flow(value: Option<PathBuf>) -> Result<PathBuf> {
    resolve_asset(
        value,
        "TALLY_CAMPAIGN_FLOW",
        "share/tally/flows/spec-build.js",
        "examples/flows/spec-build.js",
    )
}

fn resolve_driver(value: Option<PathBuf>) -> Result<PathBuf> {
    resolve_asset(
        value,
        "TALLY_CAMPAIGN_DRIVER",
        "libexec/tally/spec-build-driver",
        "drivers/spec_build_driver.py",
    )
}

fn github_repository_from_remote(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('/').trim_end_matches(".git");
    let path = value
        .strip_prefix("https://github.com/")
        .or_else(|| value.strip_prefix("ssh://git@github.com/"))
        .or_else(|| value.strip_prefix("git@github.com:"))?;
    parse_repository(path).ok()
}

/// Arming is the last moment before a campaign spends real agent time, so a
/// policy pairing the adapter cannot honour is refused here rather than three
/// seconds into the first implementation node.
fn validate_agent_policies(agent: &CampaignAgent, adapter: &AdapterConfig) -> Result<()> {
    if let Some(policy) = &agent.approval_policy {
        if !adapter.launch.approval_policies.contains_key(policy) {
            return Err(invalid(format!(
                "campaign agent approvalPolicy {policy:?} is not authorized by adapter {:?}",
                agent.adapter
            )));
        }
    }
    if let Some(policy) = &agent.sandbox_policy {
        if !adapter.launch.sandbox_policies.contains_key(policy) {
            return Err(invalid(format!(
                "campaign agent sandboxPolicy {policy:?} is not authorized by adapter {:?}",
                agent.adapter
            )));
        }
    }
    // The implementation node's whole obligation is a commit. When the adapter
    // has said which of its sandbox policies reach git metadata, that pairing is
    // knowable before any agent time is spent.
    if !adapter
        .launch
        .permits_commit(agent.sandbox_policy.as_deref())
    {
        return Err(invalid(format!(
            "campaign agent sandboxPolicy {:?} cannot create a commit under adapter {:?}; choose one of: {}",
            agent.sandbox_policy.as_deref().unwrap_or("<adapter default>"),
            agent.adapter,
            adapter.launch.commit_capable_names()
        )));
    }
    Ok(())
}

fn worker_findings_warning(agent: &CampaignAgent, adapter: &AdapterConfig) -> Option<String> {
    (!adapter.scrape.contains_key("finalMessage")).then(|| {
        format!(
            "campaign agent adapter {:?} declares no scrape.finalMessage; worker findings will not be retained",
            agent.adapter
        )
    })
}

const CACHE_USING_TOOLS: [&str; 6] = ["nix", "go", "cargo", "npm", "pip", "uv"];
const COMMON_CACHE_REDIRECTS: [&str; 2] = ["XDG_CACHE_HOME", "XDG_STATE_HOME"];
const CACHE_REDIRECTS: [&str; 9] = [
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "GOCACHE",
    "GOMODCACHE",
    "CARGO_HOME",
    "NPM_CONFIG_CACHE",
    "npm_config_cache",
    "PIP_CACHE_DIR",
    "UV_CACHE_DIR",
];

fn argv_mentions_command(argv: &[String], command: &str) -> bool {
    argv.iter().any(|argument| {
        argument
            .split(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/'))
            })
            .filter(|token| !token.is_empty())
            .any(|token| token.rsplit('/').next() == Some(command))
    })
}

fn argv_invokes_cache_using_tool(argv: &[String], tool: &str) -> bool {
    if tool != "nix" {
        return argv_mentions_command(argv, tool);
    }

    const EVALUATING_NIX_SUBCOMMANDS: [&str; 4] = ["develop", "build", "shell", "run"];
    let joined = argv.join(" ");
    joined.split([';', '&', '|']).any(|command| {
        let tokens = command
            .split(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/'))
            })
            .filter(|token| !token.is_empty())
            .collect::<Vec<_>>();
        tokens.windows(2).any(|pair| {
            pair[0].rsplit('/').next() == Some("nix")
                && EVALUATING_NIX_SUBCOMMANDS.contains(&pair[1])
        })
    })
}

fn has_nonempty_assignment(argument: &str, name: &str) -> bool {
    let assignment = format!("{name}=");
    argument.match_indices(&assignment).any(|(index, _)| {
        let boundary = argument[..index]
            .chars()
            .next_back()
            .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'));
        let value = argument[index + assignment.len()..].trim_start_matches(['\'', '"']);
        boundary
            && value.chars().next().is_some_and(|character| {
                !character.is_ascii_whitespace() && !matches!(character, ';' | '&' | '|')
            })
    })
}

fn argv_has_assignment(argv: &[String], names: &[&str]) -> bool {
    names.iter().any(|name| {
        argv.iter()
            .any(|argument| has_nonempty_assignment(argument, name))
    })
}

fn tool_cache_redirects(tool: &str) -> &'static [&'static str] {
    match tool {
        "go" => &["GOCACHE", "GOMODCACHE"],
        "cargo" => &["CARGO_HOME"],
        "npm" => &["NPM_CONFIG_CACHE", "npm_config_cache"],
        "pip" => &["PIP_CACHE_DIR"],
        "uv" => &["UV_CACHE_DIR"],
        _ => &[],
    }
}

fn has_cache_redirect(argv: &[String], tool: &str) -> bool {
    argv_has_assignment(argv, &COMMON_CACHE_REDIRECTS)
        || argv_has_assignment(argv, tool_cache_redirects(tool))
}

fn tmp_reference_is_cache_assignment(argument: &str, tmp_index: usize) -> bool {
    CACHE_REDIRECTS.iter().any(|name| {
        let assignment = format!("{name}=");
        argument[..tmp_index]
            .rmatch_indices(&assignment)
            .next()
            .is_some_and(|(index, _)| {
                let boundary = argument[..index]
                    .chars()
                    .next_back()
                    .is_none_or(|character| {
                        !(character.is_ascii_alphanumeric() || character == '_')
                    });
                let value_prefix = &argument[index + assignment.len()..tmp_index];
                boundary
                    && value_prefix
                        .chars()
                        .all(|character| matches!(character, '\'' | '"'))
            })
    })
}

fn argument_tmp_references(argument: &str) -> Vec<(usize, String)> {
    argument
        .match_indices("/tmp")
        .filter_map(|(index, _)| {
            let before = argument[..index].chars().next_back();
            let after = argument[index + "/tmp".len()..].chars().next();
            let starts_path = before.is_none_or(|character| {
                !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.' | '/'))
            });
            let ends_root = after.is_none_or(|character| {
                character == '/'
                    || character.is_ascii_whitespace()
                    || matches!(character, '\'' | '"' | ';' | ':' | ',' | ')')
            });
            if !starts_path || !ends_root || tmp_reference_is_cache_assignment(argument, index) {
                return None;
            }
            let end = argument[index..]
                .find(|character: char| {
                    character.is_ascii_whitespace()
                        || matches!(character, '\'' | '"' | ';' | ':' | ',' | ')' | '&' | '|')
                })
                .map_or(argument.len(), |offset| index + offset);
            Some((index, argument[index..end].to_owned()))
        })
        .collect()
}

fn tmp_path_was_created(created: &[String], path: &str) -> bool {
    created.iter().any(|directory| {
        path == directory
            || (directory != "/tmp"
                && path
                    .strip_prefix(directory)
                    .is_some_and(|suffix| suffix.starts_with('/')))
    })
}

fn argv_has_staged_tmp_reference(argv: &[String]) -> bool {
    let joined = argv.join(" ");
    let mut created = Vec::new();
    for command in joined.split([';', '&', '|']) {
        let references = argument_tmp_references(command);
        let creates_directories = argv_mentions_command(&[command.to_owned()], "mkdir")
            && command
                .split_ascii_whitespace()
                .any(|token| matches!(token.trim_matches(['\'', '"']), "-p" | "--parents"));
        for (_, path) in references {
            if creates_directories {
                created.push(path);
            } else if !tmp_path_was_created(&created, &path) {
                return true;
            }
        }
    }
    false
}

fn argument_has_home_reference(argument: &str) -> bool {
    argument.match_indices("$HOME").any(|(index, _)| {
        argument[index + "$HOME".len()..]
            .chars()
            .next()
            .is_none_or(|character| !(character.is_ascii_alphanumeric() || character == '_'))
    }) || argument.contains("${HOME}")
}

fn argv_appears_to_write_home(argv: &[String]) -> bool {
    if !argv
        .iter()
        .any(|argument| argument_has_home_reference(argument))
    {
        return false;
    }
    const WRITE_COMMANDS: [&str; 11] = [
        "chmod", "chown", "cp", "install", "ln", "mkdir", "mv", "tee", "touch", "truncate",
        "unlink",
    ];
    const WRITE_OPTIONS: [&str; 8] = [
        "-o",
        "--cache-dir",
        "--destination",
        "--out-dir",
        "--output",
        "--prefix",
        "--root",
        "--target-dir",
    ];
    argv.iter().any(|argument| argument.contains('>'))
        || WRITE_COMMANDS
            .iter()
            .any(|command| argv_mentions_command(argv, command))
        || argv.windows(2).any(|pair| {
            WRITE_OPTIONS.contains(&pair[0].as_str()) && argument_has_home_reference(&pair[1])
        })
        || argv.iter().any(|argument| {
            argument_has_home_reference(argument)
                && WRITE_OPTIONS
                    .iter()
                    .any(|option| argument.starts_with(&format!("{option}=")))
        })
}

fn argv_hazard_warnings(manifest: &CampaignManifest, hardening: AdapterHardening) -> Vec<String> {
    if hardening.is_none() {
        return Vec::new();
    }

    let mut warnings = Vec::new();
    let mut scan = |context: String, argv: &[String]| {
        let unredirected_tools = CACHE_USING_TOOLS
            .iter()
            .copied()
            .filter(|tool| {
                argv_invokes_cache_using_tool(argv, tool) && !has_cache_redirect(argv, tool)
            })
            .collect::<Vec<_>>();
        if !unredirected_tools.is_empty() {
            warnings.push(format!(
                "{context} invokes {} without an in-argv cache/state redirect (XDG_CACHE_HOME, XDG_STATE_HOME, or a tool-specific equivalent such as GOCACHE); it may fail under the resolved adapter's hardened tier",
                unredirected_tools.join(", ")
            ));
        }
        if argv_has_staged_tmp_reference(argv) {
            warnings.push(format!(
                "{context} references a /tmp path; PrivateTmp hides paths staged outside the transient unit"
            ));
        }
        if argv_appears_to_write_home(argv) {
            warnings.push(format!(
                "{context} appears to write through $HOME; ProtectHome=read-only can reject that write"
            ));
        }
    };

    for task in &manifest.tasks {
        if task.kind == "checkpoint" {
            if let Some(argv) = task.argv.as_deref() {
                scan(format!("checkpoint task {:?} argv", task.id), argv);
            }
        }
    }
    for gate in &manifest.gates {
        if let CampaignGate::Command {
            id,
            preflight_argv,
            argv,
            ..
        } = gate
        {
            scan(
                format!("campaign gate {id:?} preflightArgv"),
                preflight_argv,
            );
            scan(format!("campaign gate {id:?} argv"), argv);
        }
    }
    warnings
}

fn validate_host(
    graph: &CampaignGraph,
    config_path: Option<&Path>,
    flow: &Path,
    driver: &Path,
    allow_test_local_forge: bool,
) -> Result<Vec<String>> {
    let manifest = &graph.canonical.manifest;
    let config = load_client_config(config_path)?;
    let required_nodes = max_flow_nodes(manifest);
    if config.enqueue.fanout_cap < required_nodes {
        return Err(invalid(format!(
            "campaign pass requires enqueue.fanoutCap >= {required_nodes}; host has {}",
            config.enqueue.fanout_cap
        )));
    }
    for pool in ["flow", "campaign-agent", "campaign-control"] {
        if !config.pools.contains_key(pool) {
            return Err(invalid(format!(
                "forge-native campaigns require configured pool {pool:?}"
            )));
        }
    }
    validate_campaign_runner_pool(&manifest.pool, &config.pools)?;
    let mut required_adapters = vec![
        "shell",
        "spec-build-driver",
        manifest.agent.adapter.as_str(),
    ];
    // The steward is bound as a catalog role, so arming refuses a campaign
    // whose narrator names an adapter this host does not configure rather than
    // degrading every publication to the template at run time.
    if let Some(steward) = &manifest.steward {
        required_adapters.push(steward.adapter.as_str());
    }
    for adapter in required_adapters {
        if !config.adapters.contains_key(adapter) {
            return Err(invalid(format!(
                "forge-native campaigns require configured adapter {adapter:?}"
            )));
        }
    }
    let agent_adapter = &config.adapters[&manifest.agent.adapter];
    validate_agent_policies(&manifest.agent, agent_adapter)?;
    if !flow.is_file() || !driver.is_file() {
        return Err(invalid(
            "campaign flow and driver assets must be regular files",
        ));
    }
    let checkout = &manifest.repository.checkout;
    if !checkout.is_dir() {
        return Err(invalid(format!(
            "campaign repository checkout does not exist: {}",
            checkout.display()
        )));
    }
    let status = ProcessCommand::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("cannot execute git while validating campaign checkout")?;
    if !status.success() {
        return Err(invalid(format!(
            "campaign repository checkout is not a Git worktree: {}",
            checkout.display()
        )));
    }
    if manifest.repository.forge == "local" {
        if !allow_test_local_forge {
            return Err(invalid(
                "forge=local is test-only for issue campaigns; pass --allow-test-local-forge only in an isolated mechanism test",
            ));
        }
    } else {
        let remote = ProcessCommand::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["remote", "get-url", manifest.repository.remote.as_str()])
            .output()
            .context("cannot execute git while binding campaign remote")?;
        if !remote.status.success() {
            bail!(
                "cannot resolve campaign remote {:?}: {}",
                manifest.repository.remote,
                String::from_utf8_lossy(&remote.stderr).trim()
            );
        }
        let remote =
            String::from_utf8(remote.stdout).context("campaign remote URL was not valid UTF-8")?;
        let bound = github_repository_from_remote(&remote).ok_or_else(|| {
            invalid("GitHub issue campaigns require an https or SSH github.com checkout remote")
        })?;
        if !bound.eq_ignore_ascii_case(&graph.locator.repository) {
            bail!(
                "campaign issue repository {} does not match checkout remote repository {}",
                graph.locator.repository,
                bound
            );
        }
    }
    // Checkpoint and command-gate argvs run through the flow `sh` native, so
    // their hazards depend on the resolved shell adapter rather than the agent.
    let mut warnings = argv_hazard_warnings(manifest, config.adapters["shell"].hardening);
    if let Some(warning) = worker_findings_warning(&manifest.agent, agent_adapter) {
        warnings.push(warning);
    }
    Ok(warnings)
}

fn validate_campaign_runner_pool(pool: &str, pools: &BTreeMap<String, PoolConfig>) -> Result<()> {
    if pool.starts_with(CAMPAIGN_POOL_PREFIX) {
        if is_campaign_pool_name(pool) {
            return Ok(());
        }
        return Err(invalid(
            "campaign namespace pool must use campaign/OWNER/REPO form",
        ));
    }
    let runner = pools.get(pool).ok_or_else(|| {
        invalid(format!(
            "forge-native campaigns require configured pool {pool:?}"
        ))
    })?;
    if runner.resource() != ResourceKind::Mutex || runner.capacity != 1 {
        return Err(invalid(format!(
            "campaign runner pool {pool:?} must be a capacity-1 mutex"
        )));
    }
    Ok(())
}

fn priority(value: &str) -> Priority {
    match value {
        "interrupt" => Priority::Interrupt,
        "high" => Priority::High,
        "medium" => Priority::Medium,
        _ => Priority::Low,
    }
}

fn max_flow_nodes(manifest: &CampaignManifest) -> u32 {
    let command_gates = manifest
        .gates
        .iter()
        .filter(|gate| gate.is_command())
        .count();
    // Two nodes per command gate: the gating base-safe probe and the
    // non-gating witness that runs the gate's real merge-criterion argv on the
    // same pristine base. The witness decides nothing, but it is admitted and
    // therefore budgeted.
    let preflight = if command_gates == 0 {
        0
    } else {
        2 * command_gates + 2
    };
    // Sweep, reconcile, one possible continuation, and each worst-case
    // implementation lane: prep, steering re-check, agent, ownership, gates,
    // publish, rebase,
    // optional re-gates, merge, then the failure path's machinery retry, diff,
    // diagnosis, and steering, and finally cleanup. A lane that fails at merge
    // is the expensive one, not a lane that merges: maxNodes counts cumulative
    // rows, so finished nodes never return budget. Budgeting the success path
    // alone starves failure steering exactly when it is needed. A machinery
    // fault whose retry budget is already spent records the retry node and is
    // then steered, so both failure paths can land in one lane. Checkpoint
    // lanes are smaller.
    (3 + preflight + manifest.max_parallel * (12 + 2 * manifest.gates.len())) as u32
}

/// The `--projection-wait-ms` an arm may record (#432).
///
/// Refused at arm rather than at the first pass, because the value is durable:
/// a zero here would be written into the registration and then rejected by
/// every `tally flow run` this campaign ever dispatches, including the ones the
/// poll timer dispatches unattended. Absent stays absent, which is what leaves
/// the flow host's own 10 s default alone.
fn validated_projection_wait_ms(value: Option<u64>) -> Result<Option<u64>> {
    if value == Some(0) {
        return Err(invalid("--projection-wait-ms must be greater than zero"));
    }
    Ok(value)
}

/// Argv the pass writes into the events directory to admit its own successor.
///
/// It is the poll the timer already runs: one registry scan that refetches the
/// issue graph, recomputes the observation revision, and dispatches through
/// `dispatch_campaign`, so the next pass inherits the `campaign:<repo>:<number>:<revision>`
/// dedup identity. A duplicate event, or a race with `tally-campaign-poll.timer`,
/// therefore collapses in the enqueue kernel instead of starting a second pass.
/// The host bindings every dispatch needs: where the daemon listens, which
/// configuration it was started from, and which registry the pass belongs to.
#[derive(Clone, Copy)]
struct CampaignHost<'a> {
    socket: &'a Path,
    config_path: Option<&'a Path>,
    state_dir: &'a Path,
    rpc_timeout: Duration,
}

impl CampaignHost<'_> {
    /// The one global CLI prefix inherited by every campaign child.
    ///
    /// Global flags have to precede `flow`/`campaign` for clap to parse the
    /// child exactly like the host that admitted it. In particular, a NixOS
    /// host keeps its only configuration at `/etc/tally/config.json`; allowing
    /// the initial flow to fall back to the service account's XDG home made it
    /// disagree with both the daemon and the continuation poll (#442).
    fn tally_argv_prefix(&self, executable: &Path) -> Vec<String> {
        let mut argv = vec![executable.display().to_string()];
        if let Some(config) = self.config_path {
            argv.push("--config".to_owned());
            argv.push(config.display().to_string());
        }
        argv.push("--socket".to_owned());
        argv.push(self.socket.display().to_string());
        argv
    }

    /// Argv of the `tally flow run` this campaign dispatches for one pass.
    ///
    /// This is the only place a durable registration turns into something the
    /// pass actually executes, and an argv nothing constructs in a test is an
    /// argv nothing notices the loss of (#432). `projection_wait_ms` travels on
    /// the argv rather than in the environment because the pass runs as a
    /// daemon-launched transient unit whose environment is an explicit
    /// `--setenv` list, so nothing an operator exports at arm time is visible to
    /// it. `None` adds no projection-wait elements: this vector is hashed into
    /// the enqueue payload, so an unconditional flag would move every existing
    /// campaign's payload identity.
    fn dispatch_flow_argv(
        &self,
        executable: &Path,
        flow: &Path,
        max_nodes: u32,
        projection_wait_ms: Option<u64>,
    ) -> Vec<String> {
        let mut argv = self.tally_argv_prefix(executable);
        argv.extend([
            "flow".to_owned(),
            "run".to_owned(),
            flow.display().to_string(),
            "--args-from-brief".to_owned(),
            "--max-nodes".to_owned(),
            max_nodes.to_string(),
        ]);
        if let Some(millis) = projection_wait_ms {
            argv.push("--result-projection-wait-ms".to_owned());
            argv.push(millis.to_string());
        }
        argv
    }

    fn continuation_argv(&self, executable: &Path) -> Vec<String> {
        let mut argv = self.tally_argv_prefix(executable);
        argv.extend([
            "campaign".to_owned(),
            "poll".to_owned(),
            "--once".to_owned(),
            "--state-dir".to_owned(),
            self.state_dir.display().to_string(),
        ]);
        argv
    }

    fn events_dir(&self) -> PathBuf {
        self.state_dir.join("events")
    }
}

async fn dispatch_campaign(
    host: CampaignHost<'_>,
    graph: &CampaignGraph,
    steering: &CampaignSteering,
    repository_progress: &Value,
    registration: &mut CampaignRegistration,
    sub_issue_walk: bool,
    wait: bool,
) -> Result<Value> {
    let CampaignHost {
        socket,
        config_path,
        rpc_timeout,
        ..
    } = host;
    let manifest = &graph.canonical.manifest;
    if graph.canonical.executable_digest != registration.approved_graph_digest {
        bail!(
            "campaign executable graph changed from admitted {} to {}; inspect the projection and run `tally campaign arm {} {}` to approve it",
            registration.approved_graph_digest,
            graph.canonical.executable_digest,
            registration.code_repository,
            registration.worklist_pattern,
        );
    }
    let _ = validate_host(
        graph,
        config_path,
        &registration.flow,
        &registration.driver,
        true,
    )?;
    let revision = campaign_observation(
        graph,
        steering,
        repository_progress,
        registration.arm_serial,
    )?;
    let executable = std::env::current_exe().context("cannot resolve tally executable")?;
    let brief = json!({
        "campaignIdentity": &registration.registration_id,
        "repository": &graph.locator.repository,
        "issue": {
            "number": graph.locator.number.to_string(),
            "url": &graph.locator.url,
        },
        "runId": &revision,
        "worklist": {
            "kind": "github-issue",
            "graphDigest": &registration.approved_graph_digest,
        },
        // Keep #433's normalized manifest receipt at the public arm boundary.
        // Current flows carry the complete graph below; compatibility callers
        // may carry only this manifest plus its graph digest, in which case
        // the driver must reconstruct and verify the omitted task envelope.
        "armedManifest": manifest,
        // The complete graph Rust normalized and hashed. The packaged driver
        // consumes this envelope; it never reparses the issue manifest into a
        // second executable contract.
        "campaignGraph": &graph.canonical,
        "steering": steering.master,
        "taskSteering": steering.tasks,
        // The pre-dispatch task-thread re-check must use the exact authority
        // that produced both snapshots above. Inferring it from existing
        // comments would silently exclude an allowed actor who had not yet
        // commented -- precisely the late-arrival case this field serves.
        "allowedActors": &registration.allowed_actors,
        "capabilities": {"subIssueWalk": sub_issue_walk},
        "workspaceRoot": &registration.workspace_root,
        // Checkpoint snapshots join the executor's existing archive so the
        // ordinary captureArchiveHorizon sweep owns their lifecycle.
        "captureRoot": host.state_dir.join("capture/archive"),
        "tally": &executable,
        "driver": &registration.driver,
        "driverRuntimeMaxSec": manifest.driver_runtime_max_sec,
        "continuation": {
            "argv": host.continuation_argv(&executable),
            // The control pool, not the campaign mutex: the scan must be free
            // to run while this pass finishes its cleanup. Its dispatch still
            // queues behind the capacity-1 runner mutex, so passes serialize.
            "pool": ["campaign-control"],
            "priority": "low",
            "runtimeMaxSec": manifest.driver_runtime_max_sec,
            "eventsDir": host.events_dir(),
        },
    });
    let payload = EnqueuePayload {
        invocation: None,
        argv: Some(host.dispatch_flow_argv(
            &executable,
            &registration.flow,
            max_flow_nodes(manifest),
            registration.projection_wait_ms,
        )),
        pools: Some(vec!["flow".to_owned(), manifest.pool.clone()]),
        executor: None,
        priority: Some(priority(&manifest.agent.priority)),
        adapter: Some("shell".to_owned()),
        cwd: None,
        workspace: None,
        adapter_options: None,
        gate_manifest: None,
        brief: Some(brief),
        brief_path: None,
        resume_from: None,
        source: Some(EnqueueSource::Manual),
        dedup_key: Some(format!(
            "campaign:{}:{}:{}",
            graph.locator.repository, graph.locator.number, revision
        )),
        submission: Some(SubmissionOptions {
            mode: SubmissionMode::Full,
        }),
        orchestration: None,
        parent: None,
        evidence: vec!["exit:0".to_owned()],
        drv: None,
        evidence_class: Some(json!({
            "kind": "forge-native-campaign",
            "issue": &graph.locator.url,
            "registrationId": &registration.registration_id,
            "revision": &revision,
            "approvedBy": &registration.local_actor,
            "allowedActors": &registration.allowed_actors,
            "graphDigest": &registration.approved_graph_digest,
        })),
        manifest_hash: Some(graph.canonical.executable_digest.clone()),
        consumption_estimate: None,
        runtime_max_sec: manifest.runtime_max_sec,
        no_enqueue: false,
        credentials: Default::default(),
        origin: None,
        caller_job_id: inherited_caller_job_id(),
        caller_job_token: inherited_caller_job_token(),
        gh_trigger_actor: None,
        gh_self_actor: None,
        gh_origin: None,
        task_uuid: None,
        related_trigger: None,
        wait,
    };
    let client = connect_rpc(socket, config_path).await?;
    let admitted = client
        .call("queue.enqueue", Some(serde_json::to_value(payload)?))
        .await?;
    report_degraded_membership(&admitted)?;
    registration.last_observation = Some(revision);
    if !wait || admitted.get("verdict").and_then(Value::as_str).is_some() {
        return Ok(admitted);
    }
    let task_uuid = admitted
        .get("task_uuid")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid("queue.enqueue returned no task_uuid for campaign --wait"))?;
    Ok(await_job_with_rearm(client, socket, task_uuid, rpc_timeout).await?)
}

async fn run_campaign_arm(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignArmArgs,
) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let prior = registry.read_campaign(&code_repository, &worklist_pattern)?;
    if let Some(registration) = &prior {
        require_local_actor(registration)?;
    }
    let mut projection = read_campaign_projection(&state_dir, &code_repository, &worklist_pattern)?;
    let locator = projection.locator()?;
    let local_actor = local_actor();
    let forge_actor = authenticated_actor()?;
    let allowed_actors = normalize_allowed_actors(&args.allowed_actors, &forge_actor)?;
    let graph = fetch_campaign_graph(&locator)?;
    require_allowed_issue_authors(&graph, &allowed_actors)?;
    // Probe once, at arm, and record the answer. A pass never has to discover
    // mid-flight that half its projection is unavailable.
    let sub_issue_walk = probe_sub_issue_walk(&locator)?;
    projection.sub_issue_walk = Some(sub_issue_walk);
    let steering = fetch_campaign_steering(&graph, &allowed_actors, sub_issue_walk)?;
    let prior_graph = prior
        .as_ref()
        .map(|registration| read_approved_graph_snapshot(&state_dir, registration))
        .transpose()?
        .flatten();
    let escalated = if prior.is_some() && graph.canonical.manifest.repository.forge == "github" {
        active_escalated_tasks(&graph, &forge_actor, sub_issue_walk)?
    } else {
        BTreeSet::new()
    };
    let (pardon_plan, mut arm_warnings) =
        amendment_pardon_plan(prior_graph.as_ref(), &graph.canonical, &escalated);
    let flow = resolve_flow(args.flow)?;
    let driver = resolve_driver(args.driver)?;
    let workspace_root = args
        .workspace_root
        .map_or_else(default_campaign_workspace_root, Ok)?;
    if !workspace_root.is_absolute() {
        return Err(invalid("campaign workspace root must be absolute"));
    }
    let projection_wait_ms = validated_projection_wait_ms(args.projection_wait_ms)?;
    let arm_serial = prior.as_ref().map_or(Ok(1), |value| {
        value
            .arm_serial
            .checked_add(1)
            .ok_or_else(|| invalid("campaign arm retry counter is exhausted"))
    })?;
    let mut registration = CampaignRegistration::new(
        CampaignRegistrationV3 {
            schema_version: REGISTRY_SCHEMA_VERSION,
            registration_id: prior.as_ref().map_or_else(
                || uuid::Uuid::now_v7().to_string(),
                |value| value.registration_id.clone(),
            ),
            worklist_pattern: worklist_pattern.clone(),
            code_repository: code_repository.clone(),
            armed_at: Utc::now().to_rfc3339(),
            arm_serial,
            approved_graph_digest: graph.canonical.executable_digest.clone(),
            local_actor,
            allowed_actors,
            last_observation: prior
                .as_ref()
                .and_then(|value| value.last_observation.clone()),
            flow,
            driver,
            workspace_root,
        },
        projection_wait_ms,
    );
    arm_warnings.extend(validate_host(
        &graph,
        config_path,
        &registration.flow,
        &registration.driver,
        args.allow_test_local_forge,
    )?);
    let mut auto_pardons = Vec::with_capacity(pardon_plan.len());
    for pardon in &pardon_plan {
        let receipt = post_campaign_auto_pardon(&graph, &forge_actor, pardon)?;
        auto_pardons.push(AutoPardonReceipt {
            task_id: pardon.task_id.clone(),
            added_dependencies: pardon.added_dependencies.clone(),
            resume_receipt: receipt,
        });
    }
    write_campaign_projection(&state_dir, &projection)?;
    write_approved_graph_snapshot(&state_dir, &registration, &graph.canonical)?;
    registry.write(&mut registration)?;
    prune_approved_graph_snapshots(&state_dir, &registration)?;
    if args.no_enqueue {
        let receipt = json!({
            "status": "armed",
            "issue": locator.url,
            "codeRepository": code_repository,
            "worklistPattern": worklist_pattern,
            "tasks": graph.tasks.len(),
            "graphDigest": graph.canonical.executable_digest,
            "allowedActors": registration.allowed_actors,
            "enqueued": false,
        });
        outln!(
            "{}",
            serde_json::to_string(&arm_receipt(
                &receipt,
                sub_issue_walk,
                &auto_pardons,
                &arm_warnings,
            ))?
        );
        return Ok(());
    }
    let repository_progress = repository_progress_value(&graph)?;
    let result = dispatch_campaign(
        CampaignHost {
            socket,
            config_path,
            state_dir: &state_dir,
            rpc_timeout,
        },
        &graph,
        &steering,
        &repository_progress,
        &mut registration,
        sub_issue_walk,
        args.wait,
    )
    .await?;
    registry.write(&mut registration)?;
    outln!(
        "{}",
        serde_json::to_string(&arm_receipt(
            &result,
            sub_issue_walk,
            &auto_pardons,
            &arm_warnings,
        ))?
    );
    if args.wait {
        let code = waited_exit_code(&result);
        if code != 0 {
            return Err(anyhow::Error::new(ExitFailure {
                code,
                message: "campaign reconcile pass returned a non-zero verdict".to_owned(),
            }));
        }
    }
    Ok(())
}

fn post_campaign_pardon(
    graph: &CampaignGraph,
    actor: &str,
    reason: &str,
    scope: PardonScope,
) -> Result<String> {
    let reason = reason.trim();
    if reason.is_empty() || reason.chars().count() > 4_000 || reason.contains('\0') {
        return Err(invalid(
            "campaign resume --reason must contain 1..=4000 characters without NUL bytes",
        ));
    }
    let nonce = uuid::Uuid::now_v7();
    let marker = format!(
        "<!-- tally:spec-build:resume:v1 campaign={} issue={} nonce={} -->",
        graph.canonical.manifest.name, graph.locator.number, nonce
    );
    let (marker, pardon) = match scope {
        PardonScope::All => (
            marker,
            "Pardoned prior machine-diagnosis, machinery-retry, and escalation receipts without deleting the audit trail."
                .to_owned(),
        ),
        PardonScope::Tasks(tasks) => {
            if tasks.is_empty() || tasks.iter().any(|task_id| !safe_task_id(task_id)) {
                return Err(invalid("campaign pardon scope must name safe task ids"));
            }
            let joined = tasks.iter().cloned().collect::<Vec<_>>().join(",");
            let marker = marker
                .strip_suffix(" -->")
                .expect("resume marker has a fixed suffix");
            let rendered = tasks
                .iter()
                .map(|task_id| format!("`{task_id}`"))
                .collect::<Vec<_>>()
                .join(", ");
            (
                format!("{marker} tasks={joined} -->"),
                format!(
                    "Pardoned prior machine-diagnosis, machinery-retry, and escalation receipts for {rendered} without deleting the audit trail."
                ),
            )
        }
    };
    let body = format!(
        "{marker}\n\n### Campaign resumed\n\n{pardon}\n\nReason: {reason}\n\nRequested by `@{actor}`."
    );
    let output = run_gh(
        &os_arguments(&[
            "issue",
            "comment",
            &graph.locator.number.to_string(),
            "--repo",
            &graph.locator.repository,
            "--body-file",
            "-",
        ]),
        Some(&body),
    )?;
    output
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)
        .map(str::to_owned)
        .ok_or_else(|| invalid("gh issue comment returned no campaign resume receipt URL"))
}

fn post_campaign_resume(graph: &CampaignGraph, actor: &str, reason: &str) -> Result<String> {
    post_campaign_pardon(graph, actor, reason, PardonScope::All)
}

fn auto_pardon_reason(pardon: &PlannedAutoPardon) -> String {
    let shown = pardon
        .added_dependencies
        .iter()
        .take(12)
        .map(|dependency| format!("`{dependency}`"))
        .collect::<Vec<_>>();
    let remainder = pardon.added_dependencies.len().saturating_sub(shown.len());
    let dependencies = if remainder == 0 {
        shown.join(", ")
    } else {
        format!("{}, and {remainder} more", shown.join(", "))
    };
    format!(
        "Re-armed graph added dependency {} to escalated task `{}`; the amendment is the operator's structural steering response.",
        dependencies, pardon.task_id
    )
}

fn post_campaign_auto_pardon(
    graph: &CampaignGraph,
    actor: &str,
    pardon: &PlannedAutoPardon,
) -> Result<String> {
    post_campaign_pardon(
        graph,
        actor,
        &auto_pardon_reason(pardon),
        PardonScope::Tasks(BTreeSet::from([pardon.task_id.clone()])),
    )
}

async fn run_campaign_resume(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignResumeArgs,
) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let mut projection = read_campaign_projection(&state_dir, &code_repository, &worklist_pattern)?;
    let locator = projection.locator()?;
    let mut registration = registry
        .read_campaign(&code_repository, &worklist_pattern)?
        .ok_or_else(|| {
        invalid(format!(
            "campaign {code_repository}/{worklist_pattern} is not armed; arm it before attempting resume"
        ))
    })?;
    require_local_actor(&registration)?;
    let forge_actor = authenticated_actor()?;
    let graph = fetch_campaign_graph(&locator)?;
    require_allowed_issue_authors(&graph, &registration.allowed_actors)?;
    if graph.canonical.manifest.repository.forge != "github" {
        return Err(invalid(
            "campaign resume requires repository.forge=github; the local forge is test-only and has no GitHub comment boundary",
        ));
    }
    let sub_issue_walk = probe_sub_issue_walk(&locator)?;
    projection.sub_issue_walk = Some(sub_issue_walk);
    let _ = validate_host(
        &graph,
        config_path,
        &registration.flow,
        &registration.driver,
        true,
    )?;

    let next_arm_serial = registration
        .arm_serial
        .checked_add(1)
        .ok_or_else(|| invalid("campaign resume counter is exhausted"))?;
    let prior_digest = registration.approved_graph_digest.clone();
    let receipt = post_campaign_resume(&graph, &forge_actor, &args.reason)?;
    registration.arm_serial = next_arm_serial;
    registration.armed_at = Utc::now().to_rfc3339();
    registration.approved_graph_digest = graph.canonical.executable_digest.clone();
    // Publish the new authority before dispatch. Once this write succeeds, the
    // timer can recover an interrupted dispatch without comment deletion or
    // another manual state edit.
    write_campaign_projection(&state_dir, &projection)?;
    write_approved_graph_snapshot(&state_dir, &registration, &graph.canonical)?;
    registry.write(&mut registration)?;
    prune_approved_graph_snapshots(&state_dir, &registration)?;

    let steering = fetch_campaign_steering(&graph, &registration.allowed_actors, sub_issue_walk)?;
    let repository_progress = repository_progress_value(&graph)?;
    let result = dispatch_campaign(
        CampaignHost {
            socket,
            config_path,
            state_dir: &state_dir,
            rpc_timeout,
        },
        &graph,
        &steering,
        &repository_progress,
        &mut registration,
        sub_issue_walk,
        args.wait,
    )
    .await?;
    registry.write(&mut registration)?;

    let mut output = armed_projection(&result, sub_issue_walk);
    if let Some(object) = output.as_object_mut() {
        object.insert("status".to_owned(), json!("resumed"));
        object.insert("resumeReceipt".to_owned(), json!(receipt));
        object.insert("reason".to_owned(), json!(args.reason.trim()));
        object.insert("priorGraphDigest".to_owned(), json!(prior_digest));
        object.insert(
            "graphDigest".to_owned(),
            json!(registration.approved_graph_digest),
        );
    }
    outln!("{}", serde_json::to_string(&output)?);
    if args.wait {
        let code = waited_exit_code(&result);
        if code != 0 {
            return Err(anyhow::Error::new(ExitFailure {
                code,
                message: "campaign resumed, but its reconcile pass returned a non-zero verdict"
                    .to_owned(),
            }));
        }
    }
    Ok(())
}

async fn run_campaign_poll(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignPollArgs,
) -> Result<()> {
    if !args.once {
        return Err(invalid("campaign poll currently requires --once"));
    }
    // Events written by the previous release may still carry this hidden
    // argument. Accept it during the rollback window, but derive progress from
    // the same durable repository state as every public poll.
    let _legacy_continuation_token = args.continuation_token.as_deref();
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let entries = registry.registrations()?;
    let mut had_failures = false;
    for (path, mut registration) in entries {
        let projection = read_campaign_projection(
            &state_dir,
            &registration.code_repository,
            &registration.worklist_pattern,
        );
        let event_issue = projection
            .as_ref()
            .ok()
            .and_then(|value| value.locator().ok())
            .map_or_else(
                || {
                    format!(
                        "local://{}/{}",
                        registration.code_repository, registration.worklist_pattern
                    )
                },
                |locator| locator.url,
            );
        let attempt = async {
            require_local_actor(&registration)?;
            let mut projection = projection?;
            let locator = projection.locator()?;
            let sub_issue_walk = match projection.sub_issue_walk {
                Some(value) => value,
                None => {
                    let value = probe_sub_issue_walk(&locator)?;
                    projection.sub_issue_walk = Some(value);
                    write_campaign_projection(&state_dir, &projection)?;
                    value
                }
            };
            let master = fetch_issue(&locator)?;
            match master.state.as_str() {
                "closed" => {
                    // Canonical locator validation happened in `fetch_issue`.
                    // Completion is therefore allowed to clean up before any
                    registry.remove(&registration)?;
                    remove_approved_graph_snapshots(&state_dir, &registration.registration_id)?;
                    return Ok(CampaignPollAttempt::Complete);
                }
                "open" => {}
                state => bail!("campaign master issue returned unknown state {state:?}"),
            }
            let graph = campaign_graph_from(&locator, master)?;
            require_allowed_issue_authors(&graph, &registration.allowed_actors)?;
            if graph.canonical.executable_digest != registration.approved_graph_digest {
                return Ok(CampaignPollAttempt::RearmRequired {
                    approved_graph_digest: registration.approved_graph_digest.clone(),
                    live_graph_digest: graph.canonical.executable_digest.clone(),
                });
            }
            let repository_progress = repository_progress_value(&graph)?;
            let steering =
                fetch_campaign_steering(&graph, &registration.allowed_actors, sub_issue_walk)?;
            let observation = campaign_observation(
                &graph,
                &steering,
                &repository_progress,
                registration.arm_serial,
            )?;
            if registration.last_observation.as_deref() == Some(&observation) {
                return Ok(CampaignPollAttempt::Unchanged);
            }

            // The scan above crosses several forge and Git reads. Refresh the
            // public issue graph immediately before enqueue and refuse to act
            // on a mixed snapshot. Most importantly, a master that closed
            // while this poll was reading becomes completion here rather than
            // a queued pass whose driver later says it should have been open.
            let refreshed_master = fetch_issue(&locator)?;
            match refreshed_master.state.as_str() {
                "closed" => {
                    registry.remove(&registration)?;
                    remove_approved_graph_snapshots(&state_dir, &registration.registration_id)?;
                    return Ok(CampaignPollAttempt::Complete);
                }
                "open" => {}
                state => bail!("campaign master issue returned unknown state {state:?}"),
            }
            let refreshed_graph = campaign_graph_from(&locator, refreshed_master)?;
            if graph_poll_snapshot(&refreshed_graph)? != graph_poll_snapshot(&graph)? {
                return Ok(CampaignPollAttempt::Unchanged);
            }
            let result = dispatch_campaign(
                CampaignHost {
                    socket,
                    config_path,
                    state_dir: &state_dir,
                    rpc_timeout,
                },
                &graph,
                &steering,
                &repository_progress,
                &mut registration,
                sub_issue_walk,
                args.wait,
            )
            .await?;
            registry.write(&mut registration)?;
            if args.wait {
                let code = waited_exit_code(&result);
                if code != 0 {
                    return Err(anyhow::Error::new(ExitFailure {
                        code,
                        message: format!(
                            "campaign reconcile pass for {}/{} returned a non-zero verdict",
                            registration.code_repository, registration.worklist_pattern
                        ),
                    }));
                }
            }
            Ok::<_, anyhow::Error>(CampaignPollAttempt::Dispatched)
        }
        .await;
        let registration_path = path.display().to_string();
        let event = match attempt {
            Ok(CampaignPollAttempt::Dispatched) => CampaignPollEvent::new(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                CampaignPollStatus::Dispatched,
            ),
            Ok(CampaignPollAttempt::Complete) => CampaignPollEvent::complete(
                &registration.registration_id,
                &event_issue,
                &registration_path,
            ),
            Ok(CampaignPollAttempt::Unchanged) => CampaignPollEvent::new(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                CampaignPollStatus::Unchanged,
            ),
            Ok(CampaignPollAttempt::RearmRequired {
                approved_graph_digest,
                live_graph_digest,
            }) => CampaignPollEvent::graph_change(
                &registration.registration_id,
                &event_issue,
                &registration_path,
                CampaignPollStatus::RearmRequired,
                approved_graph_digest,
                live_graph_digest,
            ),
            Err(error) => {
                had_failures = true;
                CampaignPollEvent::failed(
                    &registration.registration_id,
                    &event_issue,
                    &registration_path,
                    format!("{error:#}"),
                )
            }
        };
        outln!("{}", serde_json::to_string(&event)?);
    }
    if had_failures {
        bail!("one or more armed campaigns could not be polled")
    } else {
        Ok(())
    }
}

async fn run_campaign_status(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignStatusArgs,
) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let registration = registry.read_campaign(&code_repository, &worklist_pattern)?;
    if let Some(registration) = &registration {
        require_local_actor(registration)?;
    }
    let projection = read_campaign_projection(&state_dir, &code_repository, &worklist_pattern)?;
    let locator = projection.locator()?;
    let params = json!({
        "issueUrl": locator.url,
        "registrationId": registration
            .as_ref()
            .map(|registration| registration.registration_id.as_str()),
        "latestObservation": registration
            .as_ref()
            .and_then(|registration| registration.last_observation.as_deref()),
    });
    let client = connect_rpc(socket, config_path).await?;
    let status = client
        .call_with_deadline("__campaign.status", Some(params), rpc_timeout)
        .await?;
    if args.json {
        outln!("{}", serde_json::to_string(&status)?);
        return Ok(());
    }
    print_campaign_status_human(&status)
}

fn print_campaign_status_human(status: &Value) -> Result<()> {
    let issue = status["issueUrl"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid campaign status response"))?;
    let state = status["state"].as_str().unwrap_or("unknown");
    let name = status["campaign"]
        .as_str()
        .or_else(|| status["repository"].as_str())
        .unwrap_or("campaign");
    outln!("Campaign {}  {}", compact_text(name), compact_text(state));
    outln!("  {}", compact_text(issue));
    if status["registered"].as_bool() == Some(true) {
        outln!(
            "Registration: {} (armed)",
            compact_text(status["registrationId"].as_str().unwrap_or("-"))
        );
    } else {
        outln!("Registration: inactive; resolved from durable campaign lineage");
    }
    outln!(
        "Observation: {}",
        compact_text(status["latestObservation"].as_str().unwrap_or("none"))
    );
    let Some(flow_run) = status["flowRunId"].as_str() else {
        outln!("Latest flow run: none (no reconcile pass admitted)");
        outln!("Campaign usage: no flow run admitted");
        return Ok(());
    };
    let run_count = status["flowRuns"].as_array().map_or(0, Vec::len);
    outln!(
        "Latest flow run: {} ({} pass{})",
        compact_text(flow_run),
        run_count,
        if run_count == 1 { "" } else { "es" }
    );
    print_run_body(status, None, "Campaign usage")
}

fn run_campaign_list(args: CampaignListArgs) -> Result<()> {
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let values = campaign_list_values(&registry)?;
    outln!("{}", serde_json::to_string(&values)?);
    Ok(())
}

fn campaign_list_values(registry: &CampaignRegistry) -> Result<Vec<Value>> {
    registry
        .registrations()?
        .into_iter()
        .map(|(_, registration)| {
            require_local_actor(&registration)?;
            Ok(registration.list_value()?)
        })
        .collect()
}

fn run_campaign_quiescent(args: CampaignQuiescentArgs) -> Result<()> {
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let values = campaign_list_values(&registry)?;
    if values.is_empty() {
        return Ok(());
    }

    errln!("{}", serde_json::to_string(&values)?);
    Err(exit_failure(1, String::new()))
}

fn run_campaign_disarm(args: CampaignDisarmArgs) -> Result<()> {
    let (code_repository, worklist_pattern) =
        campaign_identity(&args.code_repository, &args.worklist_pattern)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let registry = CampaignRegistry::open(&state_dir)?;
    let registration = registry.read_campaign(&code_repository, &worklist_pattern)?;
    let removed = if let Some(registration) = registration {
        require_local_actor(&registration)?;
        registry.remove(&registration)?;
        remove_approved_graph_snapshots(&state_dir, &registration.registration_id)?;
        true
    } else {
        false
    };
    outln!(
        "{}",
        serde_json::to_string(&json!({
            "codeRepository": code_repository,
            "worklistPattern": worklist_pattern,
            "disarmed": removed,
        }))?
    );
    Ok(())
}

fn read_json_document(path: &Path, context: &str) -> Result<Value> {
    let mut bytes = Vec::new();
    if path == Path::new("-") {
        std::io::stdin().read_to_end(&mut bytes)?;
    } else {
        bytes =
            fs::read(path).with_context(|| format!("cannot read {context} {}", path.display()))?;
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("{context} {} is not valid JSON", path.display()))
}

fn required_project_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    context: &str,
) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty() && !value.contains('\0'))
        .ok_or_else(|| invalid(format!("{context}.{field} must be a non-empty string")))
}

fn project_string_list(value: Option<&Value>, context: &str) -> Result<Vec<String>> {
    value.map_or_else(
        || Ok(Vec::new()),
        |value| {
            value
                .as_array()
                .ok_or_else(|| invalid(format!("{context} must be an array")))?
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    item.as_str()
                        .filter(|text| !text.is_empty() && !text.contains('\0'))
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            invalid(format!("{context}[{index}] must be a non-empty string"))
                        })
                })
                .collect()
        },
    )
}

fn render_project_task_body(
    object: &serde_json::Map<String, Value>,
    context: &str,
    checkpoint: Option<ProjectCheckpointBrief<'_>>,
) -> Result<String> {
    if let Some(body) = object.get("body") {
        return body
            .as_str()
            .filter(|body| !body.trim().is_empty() && !body.contains('\0'))
            .map(ToOwned::to_owned)
            .ok_or_else(|| invalid(format!("{context}.body must be a non-empty string")));
    }
    if let Some(checkpoint) = checkpoint {
        let argv = serde_json::to_string(checkpoint.argv)?;
        let dependencies = serde_json::to_string(checkpoint.dependencies)?;
        return Ok(format!(
            "## Checkpoint\n\nCampaign: `{}`\n\n## Gate argv\n\n    {argv}\n\n## Runtime limit\n\n{} seconds\n\n## Dependencies\n\n    {dependencies}\n",
            checkpoint.campaign_name, checkpoint.runtime_max_sec
        ));
    }
    let goal = required_project_string(object, "goal", context)?;
    let delivered = project_string_list(
        object.get("deliveredBehaviors"),
        &format!("{context}.deliveredBehaviors"),
    )?;
    let acceptance = object
        .get("acceptanceCriteria")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("{context}.acceptanceCriteria must be an array")))?;
    if delivered.is_empty() || acceptance.is_empty() {
        return Err(invalid(format!(
            "{context} without body requires non-empty deliveredBehaviors and acceptanceCriteria"
        )));
    }
    let mut body = format!("## Goal\n\n{goal}\n\n## Delivered behaviors\n");
    for item in delivered {
        body.push_str(&format!("\n- {item}"));
    }
    if let Some(read_first) = object.get("readFirst").and_then(Value::as_object) {
        body.push_str("\n\n## Read first\n");
        for field in ["specSections", "styleReferences"] {
            for item in project_string_list(
                read_first.get(field),
                &format!("{context}.readFirst.{field}"),
            )? {
                body.push_str(&format!("\n- {item}"));
            }
        }
    }
    body.push_str("\n\n## Acceptance criteria\n");
    for (index, candidate) in acceptance.iter().enumerate() {
        let item = candidate.as_object().ok_or_else(|| {
            invalid(format!(
                "{context}.acceptanceCriteria[{index}] must be an object"
            ))
        })?;
        let identifier = required_project_string(
            item,
            "id",
            &format!("{context}.acceptanceCriteria[{index}]"),
        )?;
        let description = required_project_string(
            item,
            "description",
            &format!("{context}.acceptanceCriteria[{index}]"),
        )?;
        body.push_str(&format!("\n- [ ] `{identifier}` — {description}"));
        if let Some(arguments) = item.get("argv") {
            let rendered = project_string_list(
                Some(arguments),
                &format!("{context}.acceptanceCriteria[{index}].argv"),
            )?;
            if !rendered.is_empty() {
                body.push_str(&format!(" (`{}`)", rendered.join(" ")));
            }
        }
    }
    body.push('\n');
    Ok(body)
}

fn project_tasks(document: &Value, campaign_name: &str) -> Result<Vec<ProjectTask>> {
    let object = document
        .as_object()
        .ok_or_else(|| invalid("campaign worklist must be an object"))?;
    if object.get("schemaVersion").and_then(Value::as_u64) != Some(1) {
        return Err(invalid("campaign worklist schemaVersion must equal 1"));
    }
    let candidates = object
        .get("tasks")
        .and_then(Value::as_array)
        .filter(|tasks| !tasks.is_empty() && tasks.len() <= 100)
        .ok_or_else(|| invalid("campaign worklist must contain 1..=100 tasks"))?;
    let mut prior = BTreeSet::new();
    let mut issues = BTreeSet::new();
    let mut tasks = Vec::with_capacity(candidates.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let context = format!("tasks[{index}]");
        let item = candidate
            .as_object()
            .ok_or_else(|| invalid(format!("{context} must be an object")))?;
        let kind = required_project_string(item, "kind", &context)?.to_owned();
        if !matches!(kind.as_str(), "implementation" | "checkpoint") {
            return Err(invalid(format!(
                "{context}.kind must be implementation or checkpoint"
            )));
        }
        let allowed = match kind.as_str() {
            "implementation" => BTreeSet::from([
                "id",
                "kind",
                "title",
                "body",
                "goal",
                "deliveredBehaviors",
                "readFirst",
                "acceptanceCriteria",
                "issue",
                "dependencies",
                "conflictDomains",
            ]),
            "checkpoint" => BTreeSet::from([
                "id",
                "kind",
                "title",
                "body",
                "issue",
                "dependencies",
                "argv",
                "runtimeMaxSec",
            ]),
            _ => unreachable!(),
        };
        if let Some(field) = item.keys().find(|field| !allowed.contains(field.as_str())) {
            return Err(invalid(format!(
                "{context} contains unsupported field {field:?} for kind {kind}"
            )));
        }
        let id = required_project_string(item, "id", &context)?.to_owned();
        if !safe_task_id(&id) || !prior.insert(id.clone()) {
            return Err(invalid(format!("{context}.id is invalid or duplicated")));
        }
        let title = required_project_string(item, "title", &context)?.to_owned();
        if title.len() > 300 || title.contains(['\r', '\n']) {
            return Err(invalid(format!(
                "{context}.title must fit on one line and be at most 300 bytes"
            )));
        }
        let issue =
            item.get("issue")
                .map(|value| {
                    value.as_u64().filter(|number| *number > 0).ok_or_else(|| {
                        invalid(format!("{context}.issue must be a positive integer"))
                    })
                })
                .transpose()?;
        if issue.is_some_and(|number| !issues.insert(number)) {
            return Err(invalid("campaign worklist repeats a task issue number"));
        }
        let dependencies =
            project_string_list(item.get("dependencies"), &format!("{context}.dependencies"))?;
        let mut seen_dependencies = BTreeSet::new();
        for dependency in &dependencies {
            if !prior.contains(dependency)
                || dependency == &id
                || !seen_dependencies.insert(dependency.clone())
            {
                return Err(invalid(format!(
                    "{context}.dependencies must be unique earlier task ids"
                )));
            }
        }
        let conflict_domains = if kind == "implementation" {
            item.get("conflictDomains")
                .map(|value| {
                    project_string_list(Some(value), &format!("{context}.conflictDomains"))
                })
                .transpose()?
        } else {
            None
        };
        let argv = if kind == "checkpoint" {
            let values = project_string_list(item.get("argv"), &format!("{context}.argv"))?;
            validate_argv(&values, &format!("{context}.argv"))?;
            Some(values)
        } else {
            None
        };
        let runtime_max_sec = if kind == "checkpoint" {
            Some(
                item.get("runtimeMaxSec")
                    .and_then(Value::as_u64)
                    .filter(|value| *value > 0)
                    .ok_or_else(|| {
                        invalid(format!(
                            "{context}.runtimeMaxSec must be a positive integer"
                        ))
                    })?,
            )
        } else {
            None
        };
        let checkpoint = (kind == "checkpoint").then(|| ProjectCheckpointBrief {
            campaign_name,
            argv: argv
                .as_deref()
                .expect("checkpoint argv was validated above"),
            runtime_max_sec: runtime_max_sec.expect("checkpoint runtime was validated above"),
            dependencies: &dependencies,
        });
        let body = render_project_task_body(item, &context, checkpoint)?;
        if body.chars().count() > 64_000 {
            return Err(invalid(format!(
                "{context} task brief must contain at most 64000 characters"
            )));
        }
        tasks.push(ProjectTask {
            id,
            kind,
            title,
            body,
            issue,
            dependencies,
            conflict_domains,
            argv,
            runtime_max_sec,
        });
    }
    Ok(tasks)
}

fn project_config(document: &Value, separate: Option<&Value>) -> Result<Value> {
    let config = match separate {
        Some(value) => value.clone(),
        None => document
            .get("campaign")
            .cloned()
            .ok_or_else(|| invalid("worklist requires a campaign object or --campaign-config"))?,
    };
    let mut object = config
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("campaign configuration must be an object"))?;
    object.insert("schemaVersion".to_owned(), json!(CAMPAIGN_SCHEMA_VERSION));
    object.insert("tasks".to_owned(), Value::Array(Vec::new()));
    Ok(Value::Object(object))
}

fn task_references(tasks: &[ProjectTask]) -> Result<Value> {
    Ok(Value::Array(
        tasks
            .iter()
            .map(|task| {
                let mut reference = json!({
                    "id": task.id,
                    "kind": task.kind,
                    "issue": task.issue.ok_or_else(|| invalid(format!("task {} has no projected issue", task.id)))?,
                    "dependencies": task.dependencies,
                });
                let object = reference.as_object_mut().expect("reference is an object");
                if task.kind == "implementation" {
                    if let Some(domains) = &task.conflict_domains {
                        object.insert("conflictDomains".to_owned(), json!(domains));
                    }
                } else {
                    object.insert("argv".to_owned(), json!(task.argv));
                    object.insert("runtimeMaxSec".to_owned(), json!(task.runtime_max_sec));
                }
                Ok(reference)
            })
            .collect::<Result<Vec<_>>>()?,
    ))
}

fn manifest_value(config: &Value, tasks: &[ProjectTask]) -> Result<Value> {
    let mut value = config.clone();
    value
        .as_object_mut()
        .expect("project_config returns an object")
        .insert("tasks".to_owned(), task_references(tasks)?);
    Ok(value)
}

fn upsert_managed_section(body: &str, start: &str, end: &str, content: &str) -> Result<String> {
    match (body.find(start), body.find(end)) {
        (None, None) => Ok(format!(
            "{}{}{}\n{}\n{}\n",
            body.trim_end(),
            if body.trim().is_empty() { "" } else { "\n\n" },
            start,
            content,
            end
        )),
        (Some(start_index), Some(end_index)) if start_index < end_index => {
            if body[start_index + start.len()..].matches(start).count() > 0
                || body[end_index + end.len()..].contains(end)
            {
                return Err(invalid(format!("managed section {start} is duplicated")));
            }
            let tail = end_index + end.len();
            Ok(format!(
                "{}{}\n{}\n{}{}",
                &body[..start_index],
                start,
                content,
                end,
                &body[tail..]
            ))
        }
        _ => Err(invalid(format!("managed section {start} is malformed"))),
    }
}

fn render_manifest_section(value: &Value) -> Result<String> {
    Ok(format!(
        "\n```json\n{}\n```\n",
        serde_json::to_string_pretty(value)?
    ))
}

fn render_worklist_section(tasks: &[ProjectTask], merged: &BTreeSet<String>) -> String {
    let mut output = String::from("\n");
    for task in tasks {
        let state = if merged.contains(&task.id) { "x" } else { " " };
        output.push_str(&format!(
            "- [{state}] {TASK_MARKER_PREFIX}{} --> #{} — {}\n",
            task.id,
            task.issue.expect("projection assigns every issue"),
            task.title
        ));
    }
    output
}

fn validate_worklist_projection(body: &str, manifest: &CampaignManifest) -> Result<()> {
    let section = extract_managed_section(body, WORKLIST_BEGIN, WORKLIST_END)?;
    let references = manifest
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task.issue))
        .collect::<BTreeMap<_, _>>();
    let mut seen = BTreeSet::new();
    for line in section.lines().filter(|line| !line.trim().is_empty()) {
        let line = line.trim();
        if !["- [ ] ", "- [x] ", "- [X] "]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        {
            return Err(invalid(
                "campaign worklist must contain only task checkbox lines",
            ));
        }
        let id = extract_task_marker(line)
            .ok_or_else(|| invalid("campaign worklist contains an invalid task marker"))?;
        let issue = references
            .get(id.as_str())
            .ok_or_else(|| invalid(format!("campaign worklist names unknown task {id}")))?;
        if !seen.insert(id.clone()) {
            return Err(invalid(format!(
                "campaign worklist repeats task marker {id}"
            )));
        }
        if !line.contains(&format!("--> #{issue}")) {
            return Err(invalid(format!(
                "campaign worklist task {id} does not name issue #{issue}"
            )));
        }
    }
    let expected = references.keys().copied().collect::<BTreeSet<_>>();
    let actual = seen.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(invalid(
            "campaign worklist task markers differ from the manifest task set",
        ));
    }
    Ok(())
}

fn render_master_body(
    base: &str,
    config: &Value,
    tasks: &[ProjectTask],
    merged: &BTreeSet<String>,
) -> Result<String> {
    let value = manifest_value(config, tasks)?;
    let with_manifest = upsert_managed_section(
        base,
        CAMPAIGN_BEGIN,
        CAMPAIGN_END,
        &render_manifest_section(&value)?,
    )?;
    upsert_managed_section(
        &with_manifest,
        WORKLIST_BEGIN,
        WORKLIST_END,
        &render_worklist_section(tasks, merged),
    )
}

fn extract_task_marker(body: &str) -> Option<String> {
    let start = body.find(TASK_MARKER_PREFIX)? + TASK_MARKER_PREFIX.len();
    let end = body[start..].find(" -->")? + start;
    let id = &body[start..end];
    safe_task_id(id).then(|| id.to_owned())
}

fn validate_issue_title(value: &str, context: &str) -> Result<()> {
    if value.trim().is_empty() || value.chars().count() > 300 || value.chars().any(char::is_control)
    {
        return Err(invalid(format!(
            "{context} must be a non-empty single-line title of at most 300 characters"
        )));
    }
    Ok(())
}

fn validate_label_name(label: &str) -> Result<()> {
    if label.trim().is_empty() || label.chars().any(char::is_control) {
        return Err(invalid(
            "campaign labels must be non-empty single-line strings",
        ));
    }
    Ok(())
}

fn ensure_label(repository: &str, label: &str, color: &str, description: &str) -> Result<()> {
    validate_label_name(label)?;
    let labels: Vec<Value> = gh_json(&[
        "label".into(),
        "list".into(),
        "--repo".into(),
        repository.into(),
        "--limit".into(),
        "1000".into(),
        "--json".into(),
        "name".into(),
    ])?;
    if labels
        .iter()
        .any(|value| value.get("name").and_then(Value::as_str) == Some(label))
    {
        return Ok(());
    }
    run_gh(
        &[
            "label".into(),
            "create".into(),
            label.into(),
            "--repo".into(),
            repository.into(),
            "--color".into(),
            color.into(),
            "--description".into(),
            description.into(),
        ],
        None,
    )?;
    Ok(())
}

fn gh_issue_number(output: &str) -> Result<u64> {
    output
        .lines()
        .rev()
        .find_map(|line| parse_issue_url(line.trim()).ok())
        .map(|locator| locator.number)
        .ok_or_else(|| invalid("gh issue create returned no canonical issue URL"))
}

fn edit_master(
    repository: &str,
    number: u64,
    title: Option<&str>,
    label: &str,
    body: &str,
) -> Result<()> {
    let mut arguments = vec![
        "issue".into(),
        "edit".into(),
        number.to_string().into(),
        "--repo".into(),
        repository.into(),
        "--body-file".into(),
        "-".into(),
        "--add-label".into(),
        label.into(),
    ];
    if let Some(title) = title {
        arguments.extend(["--title".into(), title.into()]);
    }
    run_gh(&arguments, Some(body))?;
    Ok(())
}

fn merged_project_tasks(
    repository: &str,
    manifest: &CampaignManifest,
    tasks: &[ProjectTask],
    issue_number: u64,
) -> Result<BTreeSet<String>> {
    if manifest.repository.forge != "github" {
        return Ok(BTreeSet::new());
    }
    let candidates: Vec<Value> = gh_json(&[
        "pr".into(),
        "list".into(),
        "--repo".into(),
        repository.into(),
        "--state".into(),
        "merged".into(),
        "--limit".into(),
        "1000".into(),
        "--json".into(),
        "body,baseRefName,headRefName,url".into(),
    ])?;
    let mut merged = BTreeSet::new();
    let mut claimed_urls = BTreeSet::new();
    let graph_digest = sha256_json(&json!({
        "manifest": manifest,
        "tasks": tasks.iter().map(|task| json!({
            "number": task.issue.expect("projection assigns every issue"),
            "title": task.title,
            "body": format!("{TASK_MARKER_PREFIX}{} -->\n\n{}", task.id, task.body.trim()),
        })).collect::<Vec<_>>(),
    }))?;
    for task in &manifest.tasks {
        if task.kind == "checkpoint" {
            let reference =
                checkpoint_reference_prefix(&manifest.name, issue_number, &task.id, &graph_digest)?;
            if projected_checkpoint_complete(manifest, &reference)? {
                merged.insert(task.id.clone());
            }
            continue;
        }
        let projected = tasks
            .iter()
            .find(|projected| projected.id == task.id)
            .expect("validated manifest and projected tasks have the same ids");
        let content = CanonicalCampaignTaskV1 {
            number: projected.issue.expect("projection assigns every issue"),
            title: projected.title.clone(),
            body: format!(
                "{TASK_MARKER_PREFIX}{} -->\n\n{}",
                projected.id,
                projected.body.trim()
            ),
        };
        let revision = task_completion_revision(manifest, task, &content)?;
        let marker = format!(
            "<!-- tally:spec-build:v2 campaign={} issue={} task={} revision={} -->",
            manifest.name, issue_number, task.id, revision
        );
        let branch = stable_publish_branch(&manifest.name, issue_number, &task.id, &revision);
        let mut matching = candidates
            .iter()
            .filter(|candidate| {
                candidate
                    .get("body")
                    .and_then(Value::as_str)
                    .is_some_and(|body| body.contains(&marker))
            })
            .collect::<Vec<_>>();
        let branch_candidates;
        if matching.is_empty() {
            branch_candidates = gh_json::<Vec<Value>>(&[
                "pr".into(),
                "list".into(),
                "--repo".into(),
                repository.into(),
                "--head".into(),
                branch.clone().into(),
                "--state".into(),
                "merged".into(),
                "--limit".into(),
                "2".into(),
                "--json".into(),
                "body,baseRefName,headRefName,url".into(),
            ])?;
            matching = branch_candidates
                .iter()
                .filter(|candidate| {
                    candidate
                        .get("body")
                        .and_then(Value::as_str)
                        .is_some_and(|body| body.contains(&marker))
                })
                .collect();
        }
        if matching.len() > 1 {
            bail!(
                "multiple merged pull requests claim campaign task {}",
                task.id
            );
        }
        let Some(candidate) = matching.first() else {
            continue;
        };
        let url = candidate
            .get("url")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("merged pull request for {} has no URL", task.id)))?;
        if !claimed_urls.insert(url.to_owned()) {
            bail!("merged pull request {url} claims more than one campaign task");
        }
        if candidate.get("baseRefName").and_then(Value::as_str)
            != Some(&manifest.repository.base_branch)
        {
            bail!(
                "merged pull request {url} does not target campaign base {}",
                manifest.repository.base_branch
            );
        }
        if candidate.get("headRefName").and_then(Value::as_str) != Some(branch.as_str()) {
            bail!("merged pull request {url} does not use stable task branch {branch}");
        }
        merged.insert(task.id.clone());
    }
    Ok(merged)
}

/// Has this checkpoint published a receipt the base branch already contains?
///
/// `prefix` names the task's receipt family, not one ref: the driver appends
/// the tested base revision as a final path component, and this projection —
/// which runs from a manifest, not from a reconcile pass — does not know which
/// revision that was. Every receipt under the family is considered and one
/// that the base branch contains is enough; the driver's own exact-revision
/// check remains the completion oracle, and this is the checkbox the reader
/// sees.
fn projected_checkpoint_complete(manifest: &CampaignManifest, prefix: &str) -> Result<bool> {
    let git = |arguments: &[&str]| -> Result<std::process::Output> {
        ProcessCommand::new("git")
            .arg("-C")
            .arg(&manifest.repository.checkout)
            .args(arguments)
            .output()
            .context("cannot query projected checkpoint completion")
    };
    let pattern = format!("{prefix}/*");
    let listed = git(&["ls-remote", "--refs", &manifest.repository.remote, &pattern])?;
    if !listed.status.success() {
        bail!(
            "cannot query checkpoint refs {pattern}: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        );
    }
    let stdout = String::from_utf8(listed.stdout).context("git ls-remote was not UTF-8")?;
    let mut receipts = Vec::new();
    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let (target, name) = line.split_once('\t').ok_or_else(|| {
            invalid(format!(
                "checkpoint refs {pattern} returned malformed output"
            ))
        })?;
        if !name.starts_with(&format!("{prefix}/")) {
            bail!("checkpoint refs {pattern} returned unrelated ref {name}");
        }
        if !((40..=64).contains(&target.len())
            && target.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(invalid(format!(
                "checkpoint ref {name} returned malformed output"
            )));
        }
        receipts.push((target.to_owned(), name.to_owned()));
    }
    if receipts.is_empty() {
        return Ok(false);
    }
    let fetched_base = git(&["fetch", "--prune", "--no-tags", &manifest.repository.remote])?;
    if !fetched_base.status.success() {
        bail!("cannot refresh campaign base while projecting checkpoint state");
    }
    let base = format!(
        "{}/{}",
        manifest.repository.remote, manifest.repository.base_branch
    );
    for (target, name) in receipts {
        let fetched = git(&["fetch", "--no-tags", &manifest.repository.remote, &name])?;
        if !fetched.status.success() {
            bail!("cannot fetch checkpoint ref {name}");
        }
        let object_type = git(&["cat-file", "-t", &target])?;
        if !object_type.status.success()
            || String::from_utf8_lossy(&object_type.stdout).trim() != "commit"
        {
            continue;
        }
        if git(&["merge-base", "--is-ancestor", &target, &base])?
            .status
            .success()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn stable_publish_branch(
    campaign: &str,
    issue_number: u64,
    task_id: &str,
    revision: &str,
) -> String {
    let slug = campaign.trim_matches(['.', '-']);
    let slug = &slug[..slug.len().min(32)];
    let revision = revision.strip_prefix("sha256:").unwrap_or(revision);
    let suffix = format!("-{}", &revision[..revision.len().min(16)]);
    format!("tally/{slug}-issue-{issue_number}/{task_id}{suffix}")
}

/// The family of refs one checkpoint's receipts are published under.
///
/// The driver appends `/<baseRevision>` to this prefix for the revision it
/// actually tested (`checkpoint_ref` in `spec_build_driver.py`); the shared
/// vectors in `test/fixtures/spec-build/checkpoint-refs.json` pin the two
/// layouts together from both languages.
fn checkpoint_reference_prefix(
    campaign: &str,
    issue_number: u64,
    task_id: &str,
    source: &str,
) -> Result<String> {
    let digest = source
        .strip_prefix("sha256:")
        .filter(|value| value.len() == 64)
        .ok_or_else(|| invalid("checkpoint source is not a SHA-256 identity"))?;
    Ok(format!(
        "{}/checkpoint/{task_id}-{digest}",
        campaign_state_ref_prefix(campaign, issue_number),
    ))
}

fn validate_project_shape(config: &Value, tasks: &[ProjectTask]) -> Result<CampaignManifest> {
    let mut projected = tasks.to_vec();
    let mut used = projected
        .iter()
        .filter_map(|task| task.issue)
        .collect::<BTreeSet<_>>();
    let mut placeholder = 1u64;
    for task in &mut projected {
        if task.issue.is_none() {
            while used.contains(&placeholder) {
                placeholder = placeholder
                    .checked_add(1)
                    .ok_or_else(|| invalid("campaign projection exhausted issue placeholders"))?;
            }
            task.issue = Some(placeholder);
            used.insert(placeholder);
        }
    }
    let value = manifest_value(config, &projected)?;
    admit_manifest_value(value).map_err(|error| {
        invalid(format!(
            "campaign configuration cannot form a valid manifest: {error}"
        ))
    })
}

fn reconcile_dependencies(repository: &str, tasks: &[ProjectTask]) -> Result<()> {
    let campaign_numbers = tasks
        .iter()
        .filter_map(|task| task.issue)
        .collect::<BTreeSet<_>>();
    let by_id = tasks
        .iter()
        .map(|task| (&task.id, task.issue.unwrap()))
        .collect::<BTreeMap<_, _>>();
    for task in tasks {
        let number = task.issue.unwrap();
        let viewed: Value = gh_json(&[
            "issue".into(),
            "view".into(),
            number.to_string().into(),
            "--repo".into(),
            repository.into(),
            "--json".into(),
            "blockedBy".into(),
        ])?;
        let current = viewed
            .pointer("/blockedBy/nodes")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|value| value.get("number").and_then(Value::as_u64))
            .filter(|number| campaign_numbers.contains(number))
            .collect::<BTreeSet<_>>();
        let desired = task
            .dependencies
            .iter()
            .map(|id| by_id[id])
            .collect::<BTreeSet<_>>();
        let additions = desired.difference(&current).copied().collect::<Vec<_>>();
        let removals = current.difference(&desired).copied().collect::<Vec<_>>();
        if additions.is_empty() && removals.is_empty() {
            continue;
        }
        let mut arguments = vec![
            "issue".into(),
            "edit".into(),
            number.to_string().into(),
            "--repo".into(),
            repository.into(),
        ];
        if !additions.is_empty() {
            arguments.extend([
                "--add-blocked-by".into(),
                additions
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
                    .into(),
            ]);
        }
        if !removals.is_empty() {
            arguments.extend([
                "--remove-blocked-by".into(),
                removals
                    .iter()
                    .map(u64::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
                    .into(),
            ]);
        }
        run_gh(&arguments, None)?;
    }
    Ok(())
}

fn run_campaign_project(args: CampaignProjectArgs) -> Result<()> {
    let repository = parse_repository(&args.repo)?;
    let state_dir = resolve_state_dir(args.state_dir.clone())?;
    let document = read_json_document(&args.worklist, "campaign worklist")?;
    let separate = args
        .campaign_config
        .as_deref()
        .map(|path| read_json_document(path, "campaign configuration"))
        .transpose()?;
    let raw_config = project_config(&document, separate.as_ref())?;
    let name = raw_config
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| safe_component(value))
        .ok_or_else(|| invalid("campaign configuration name is missing or invalid"))?;
    let mut tasks = project_tasks(&document, name)?;
    let canonical_preview = validate_project_shape(&raw_config, &tasks)?;
    let (worklist_pattern, source_revision, worklist_sha256) =
        committed_worklist_coordinate(&args.worklist, &canonical_preview)?;
    let mut config = serde_json::to_value(canonical_preview)?;
    config
        .as_object_mut()
        .expect("CampaignManifest serializes as an object")
        .remove("tasks");
    let initial_title = args
        .title
        .clone()
        .unwrap_or_else(|| format!("{name}: tally campaign"));
    validate_issue_title(&initial_title, "campaign master title")?;
    validate_label_name(&args.label)?;
    validate_label_name(&args.task_label)?;
    let existing_master = args
        .issue
        .as_deref()
        .map(|url| {
            let locator = parse_issue_url(url)?;
            if locator.repository != repository {
                return Err(invalid(
                    "--issue and --repo must identify the same repository",
                ));
            }
            let master = fetch_issue(&locator)?;
            if master.state != "open" {
                return Err(invalid("campaign master issue must be open"));
            }
            let body = master.body.unwrap_or_default();
            let preview = upsert_managed_section(
                &body,
                CAMPAIGN_BEGIN,
                CAMPAIGN_END,
                "\nprojection preview\n",
            )?;
            upsert_managed_section(
                &preview,
                WORKLIST_BEGIN,
                WORKLIST_END,
                "\nprojection preview\n",
            )?;
            Ok((locator, body))
        })
        .transpose()?;
    ensure_label(
        &repository,
        &args.label,
        "8250DF",
        "Forge-native tally campaign",
    )?;
    ensure_label(
        &repository,
        &args.task_label,
        "C5DEF5",
        "Task in a forge-native tally campaign",
    )?;

    let (locator, mut master_body) = if let Some(existing) = existing_master {
        existing
    } else {
        let partial = render_master_body(
            "This issue is the durable container for a forge-native tally campaign.\n",
            &config,
            &[],
            &BTreeSet::new(),
        )?;
        let output = run_gh(
            &[
                "issue".into(),
                "create".into(),
                "--repo".into(),
                repository.clone().into(),
                "--title".into(),
                initial_title.clone().into(),
                "--body-file".into(),
                "-".into(),
                "--label".into(),
                args.label.clone().into(),
            ],
            Some(&partial),
        )?;
        let number = gh_issue_number(&output)?;
        let url = format!("https://github.com/{repository}/issues/{number}");
        (parse_issue_url(&url)?, partial)
    };

    let mut known = BTreeMap::<String, u64>::new();
    if let Ok(section) = extract_managed_section(&master_body, CAMPAIGN_BEGIN, CAMPAIGN_END) {
        if let Some(json) = section
            .strip_prefix("```json")
            .and_then(|value| value.strip_suffix("```"))
        {
            if let Ok(value) = serde_json::from_str::<Value>(json.trim()) {
                if let Some(references) = value.get("tasks").and_then(Value::as_array) {
                    for reference in references {
                        if let (Some(id), Some(issue)) = (
                            reference.get("id").and_then(Value::as_str),
                            reference.get("issue").and_then(Value::as_u64),
                        ) {
                            if safe_task_id(id) && issue > 0 {
                                known.insert(id.to_owned(), issue);
                            }
                        }
                    }
                }
            }
        }
    }
    let desired_ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<BTreeSet<_>>();
    let explicit_ids = tasks
        .iter()
        .filter_map(|task| task.issue.map(|issue| (issue, task.id.clone())))
        .collect::<BTreeMap<_, _>>();
    let mut native_numbers = BTreeSet::new();
    for issue in fetch_subissues(&locator)? {
        native_numbers.insert(issue.number);
        let marker_id = extract_task_marker(issue.body.as_deref().unwrap_or_default());
        let explicit_id = explicit_ids.get(&issue.number);
        let id = match (marker_id, explicit_id) {
            (Some(marker), Some(explicit)) if marker != *explicit => {
                return Err(invalid(format!(
                    "native sub-issue #{} is marked for task {marker} but explicitly assigned to {explicit}",
                    issue.number
                )));
            }
            (Some(marker), _) => marker,
            (None, Some(explicit)) => explicit.clone(),
            (None, None) => {
                return Err(invalid(format!(
                    "native sub-issue #{} is neither owned by this tally projection nor explicitly assigned in the worklist",
                    issue.number
                )));
            }
        };
        if !desired_ids.contains(id.as_str()) {
            run_gh(
                &[
                    "issue".into(),
                    "edit".into(),
                    issue.number.to_string().into(),
                    "--repo".into(),
                    repository.clone().into(),
                    "--remove-parent".into(),
                ],
                None,
            )?;
            native_numbers.remove(&issue.number);
            known.remove(&id);
            continue;
        }
        if known
            .insert(id.clone(), issue.number)
            .is_some_and(|prior| prior != issue.number)
        {
            bail!("more than one native sub-issue claims campaign task {id}");
        }
    }
    for index in 0..tasks.len() {
        let task = &mut tasks[index];
        match (task.issue, known.get(&task.id).copied()) {
            (Some(explicit), Some(existing)) if explicit != existing => {
                return Err(invalid(format!(
                    "task {} issue {} conflicts with projected issue {}",
                    task.id, explicit, existing
                )));
            }
            (None, Some(existing)) => task.issue = Some(existing),
            _ => {}
        }
        let projected_body = format!(
            "{TASK_MARKER_PREFIX}{} -->\n\n{}",
            task.id,
            task.body.trim()
        );
        if let Some(number) = task.issue {
            let mut arguments = vec![
                "issue".into(),
                "edit".into(),
                number.to_string().into(),
                "--repo".into(),
                repository.clone().into(),
                "--title".into(),
                task.title.clone().into(),
                "--body-file".into(),
                "-".into(),
                "--add-label".into(),
                args.task_label.clone().into(),
            ];
            if !native_numbers.contains(&number) {
                arguments.extend(["--parent".into(), locator.number.to_string().into()]);
            }
            run_gh(&arguments, Some(&projected_body))?;
            native_numbers.insert(number);
        } else {
            let output = run_gh(
                &[
                    "issue".into(),
                    "create".into(),
                    "--repo".into(),
                    repository.clone().into(),
                    "--title".into(),
                    task.title.clone().into(),
                    "--body-file".into(),
                    "-".into(),
                    "--label".into(),
                    args.task_label.clone().into(),
                    "--parent".into(),
                    locator.number.to_string().into(),
                ],
                Some(&projected_body),
            )?;
            task.issue = Some(gh_issue_number(&output)?);
        }
        let projected = tasks[..=index].to_vec();
        master_body = render_master_body(&master_body, &config, &projected, &BTreeSet::new())?;
        edit_master(&repository, locator.number, None, &args.label, &master_body)?;
    }

    let final_value = manifest_value(&config, &tasks)?;
    let manifest = admit_manifest_value(final_value).map_err(|error| {
        invalid(format!(
            "projected campaign configuration is invalid: {error}"
        ))
    })?;
    validate_manifest(&manifest)?;
    reconcile_dependencies(&repository, &tasks)?;
    let merged = merged_project_tasks(&repository, &manifest, &tasks, locator.number)?;
    master_body = render_master_body(&master_body, &config, &tasks, &merged)?;
    edit_master(
        &repository,
        locator.number,
        args.title
            .as_deref()
            .or((args.issue.is_none()).then_some(initial_title.as_str())),
        &args.label,
        &master_body,
    )?;
    let projection = CampaignProjectionV1 {
        schema_version: CAMPAIGN_PROJECTION_SCHEMA_VERSION,
        code_repository: repository.clone(),
        worklist_pattern: worklist_pattern.clone(),
        source_revision,
        worklist_sha256,
        issue: Some(ProjectedIssueV1 {
            repository: locator.repository.clone(),
            number: locator.number,
            url: locator.url.clone(),
        }),
        sub_issue_walk: None,
    };
    write_campaign_projection(&state_dir, &projection)?;
    outln!(
        "{}",
        serde_json::to_string(&json!({
            "issue": locator.url,
            "codeRepository": repository,
            "worklistPattern": worklist_pattern,
            "tasks": tasks.iter().map(|task| json!({"id": task.id, "issue": task.issue})).collect::<Vec<_>>(),
            "merged": merged,
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tally_core::campaign_contract::{BRIEF_SENTINEL, DEFAULT_AGENT_SANDBOX_POLICY};

    /// The one shared checkpoint-ref vector file, asserted from here and from
    /// `test/spec_build_checkpoint_receipts_test.py`.
    ///
    /// Two languages computing the same ref name with nothing pinning them
    /// together is how the projection came to read a namespace the driver has
    /// never written: it built the receipt family and queried it as if it were
    /// the ref. Neither side owns this file; both are checked against it.
    #[test]
    fn checkpoint_ref_layout_matches_the_shared_driver_vectors() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../test/fixtures/spec-build/checkpoint-refs.json");
        let document: Value = serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display())),
        )
        .expect("checkpoint ref vectors must be JSON");
        assert_eq!(document["schemaVersion"], json!(1));
        let vectors = document["vectors"]
            .as_array()
            .expect("checkpoint ref vectors must be a list");
        assert!(!vectors.is_empty());
        for vector in vectors {
            let campaign = vector["campaign"].as_str().unwrap();
            let issue_number = vector["issueNumber"].as_u64().unwrap();
            let task_id = vector["taskId"].as_str().unwrap();
            let source = vector["source"].as_str().unwrap();
            let base_revision = vector["baseRevision"].as_str().unwrap();

            let prefix =
                checkpoint_reference_prefix(campaign, issue_number, task_id, source).unwrap();
            assert_eq!(prefix, vector["refPrefix"].as_str().unwrap());

            // The prefix has to be exactly the driver's ref minus the tested
            // revision, or the `<prefix>/*` query the projection runs matches
            // nothing the driver ever published.
            assert_eq!(
                vector["ref"].as_str().unwrap(),
                format!("{prefix}/{base_revision}")
            );
        }
    }

    fn manifest_value_for_test(tasks: Value) -> Value {
        json!({
            "schemaVersion": 1,
            "name": "night-build",
            "repository": {
                "checkout": "/tmp/example",
                "baseBranch": "main",
                "remote": "origin",
                "forge": "github"
            },
            "maxTasks": 4,
            "maxParallel": 1,
            "agent": {},
            "gates": [{
                "kind": "command",
                "id": "test",
                "preflightArgv": ["true"],
                "argv": ["true"]
            }],
            "tasks": tasks
        })
    }

    /// `TALLY_GH_PROGRAM` is process-global, so every test that swaps the
    /// `gh` binary has to hold this. Without it a sibling test's fake `gh`
    /// answers this one's calls, which is a flake, not a finding.
    static GH_PROGRAM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Restores whatever `TALLY_GH_PROGRAM` was on drop, panic or not.
    struct GhProgramGuard {
        previous: Option<OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl GhProgramGuard {
        fn acquire() -> Self {
            let lock = GH_PROGRAM_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self {
                previous: std::env::var_os("TALLY_GH_PROGRAM"),
                _lock: lock,
            }
        }

        fn use_program(&self, program: &Path) {
            std::env::set_var("TALLY_GH_PROGRAM", program);
        }
    }

    impl Drop for GhProgramGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("TALLY_GH_PROGRAM", value),
                None => std::env::remove_var("TALLY_GH_PROGRAM"),
            }
        }
    }

    /// The immutable executable this tree installs instead of writing one
    /// (#117). It reads its behaviour from a sibling script file, so the file
    /// the kernel is asked to `execve` is a checked-in fixture that no test
    /// ever opens for writing.
    const SHELL_COMMAND_PROVIDER: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test/fixtures/shell-command-provider"
    );

    fn shell_program_source(path: &Path) -> PathBuf {
        let mut source = OsString::from(path.as_os_str());
        source.push(".tally-test-script");
        PathBuf::from(source)
    }

    /// Install a fake `gh` at `directory/name` that runs `body`.
    ///
    /// This used to write the script itself and then `chmod +x` it, which is a
    /// load-dependent `ETXTBSY` race and red-gated an innocent sha (#396):
    /// `fs::write` holds a write fd across its open/write/close, and a sibling
    /// thread of a parallel test binary that `fork`s inside that window carries
    /// the fd into its child until that child reaches `execve` (`O_CLOEXEC`
    /// closes it there, not at `fork`). While any process holds a write fd on a
    /// file, the kernel refuses to execute it — `Text file busy`.
    ///
    /// Publishing the behaviour through a non-executable sidecar removes the
    /// race rather than retrying through it: the executed path is a symlink to
    /// the checked-in provider, which is never written, so the window in which
    /// the exec target is open for writing never opens at all. The script the
    /// provider reads may be held open for writing by anyone — reading a file
    /// is not an exec of it.
    fn fake_gh(directory: &Path, name: &str, body: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(shell_program_source(&path), format!("#!/bin/sh\n{body}\n")).unwrap();
        std::os::unix::fs::symlink(SHELL_COMMAND_PROVIDER, &path).unwrap();
        path
    }

    /// The race #396 filed, made deterministic, on both shapes.
    ///
    /// Under load the window is a fork landing between `fs::write`'s open and
    /// close; here the write fd is simply held open, which is the same state
    /// the kernel refuses on. The first half asserts the hazard is real rather
    /// than theorised — a written-then-`chmod`ed program is refused with
    /// `ETXTBSY` while a write fd is open on it. The second asserts the shape
    /// `fake_gh` now uses is immune under exactly that condition, because what
    /// gets executed is the checked-in provider and the file held open is only
    /// ever *read*.
    ///
    /// This is deliberately not "the suite is still green": the suite was green
    /// on the sha this race red-gated.
    #[test]
    fn a_written_program_is_unexecutable_while_open_for_writing_and_a_provided_one_is_not() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().unwrap();

        // The shape `fake_gh` used to have.
        let written = temporary.path().join("gh-written");
        let mut open_for_writing = fs::File::create(&written).unwrap();
        open_for_writing.write_all(b"#!/bin/sh\nexit 0\n").unwrap();
        open_for_writing.flush().unwrap();
        fs::set_permissions(&written, fs::Permissions::from_mode(0o755)).unwrap();
        let refused = std::process::Command::new(&written)
            .status()
            .expect_err("a program still open for writing must not be executable");
        assert_eq!(
            refused.raw_os_error(),
            Some(libc::ETXTBSY),
            "expected Text file busy, got: {refused}"
        );
        drop(open_for_writing);
        // Deliberately not asserted here: that the same program is executable
        // once the fd is closed. It usually is, and that is exactly why the
        // original failure only ever showed up under load — but this binary's
        // other threads are forking the whole time, and any of those forks
        // landing in the window above inherits a write fd on this file and
        // makes the retry fail too. Asserting it would rebuild the flake.

        // The shape it has now. Executing while the *script* is held open for
        // writing is necessary but nowhere near sufficient — that also passes
        // for a written-then-chmoded target that simply is not open right now,
        // which is the whole race. So the property itself is asserted below.
        let provided = fake_gh(temporary.path(), "gh-provided", "exit 0");
        let source = shell_program_source(&provided);
        let held_open = fs::OpenOptions::new().write(true).open(&source).unwrap();
        let status = std::process::Command::new(&provided)
            .status()
            .expect("a provided program must execute while its script is open for writing");
        assert!(status.success(), "{status}");
        drop(held_open);

        assert_exec_target_is_never_written(temporary.path(), &provided);
    }

    /// The property #396 actually rests on, asserted rather than approximated.
    ///
    /// `ETXTBSY` is raised for an exec target some process holds open for
    /// writing. What makes the converted helpers immune is not that the window
    /// is small — it is that the exec target is a file this process never opens
    /// at all: a symlink to the checked-in provider, outside the directory the
    /// test writes into. The only thing installed here that *is* written is the
    /// sidecar, and `/bin/sh` merely reads it.
    ///
    /// Asserting the exec succeeds is not this property. A written-then-chmoded
    /// program that happens not to be open at that instant also executes, which
    /// is exactly how the race stayed invisible on a quiet host.
    fn assert_exec_target_is_never_written(written_root: &Path, program: &Path) {
        use std::os::unix::fs::PermissionsExt as _;

        let installed = fs::symlink_metadata(program).unwrap();
        assert!(
            installed.file_type().is_symlink(),
            "the exec target must be a symlink to the checked-in provider, not a file \
             this process wrote: {}",
            program.display()
        );
        let target = fs::read_link(program).unwrap();
        assert!(
            target.ends_with("test/fixtures/shell-command-provider"),
            "unexpected provider target {}",
            target.display()
        );
        assert!(
            !target.starts_with(written_root),
            "the exec target resolves inside the directory this test writes into ({}), \
             so it is a file this process can hold open for writing",
            target.display()
        );
        assert!(target.exists(), "{} is not checked in", target.display());
        let sidecar = shell_program_source(program);
        assert_eq!(
            fs::metadata(&sidecar).unwrap().permissions().mode() & 0o111,
            0,
            "the file the installer writes must never be executable, or it becomes an \
             exec target this process wrote: {}",
            sidecar.display()
        );
    }

    const WALK_PAYLOAD: &str = r#"{"data":{"repository":{"issue":{"subIssues":{
        "pageInfo":{"hasNextPage":false,"endCursor":null},
        "nodes":[{"number":8,
          "closedByPullRequestsReferences":{"nodes":[]},
          "comments":{"pageInfo":{"hasPreviousPage":false},"nodes":[
            {"databaseId":11,"url":"https://github.com/acme/widgets/issues/8#c11",
             "body":"rerun the gate with the fixture regenerated",
             "createdAt":"2026-08-01T10:00:00Z","updatedAt":"2026-08-01T10:00:00Z",
             "author":{"login":"Operator"}},
            {"databaseId":12,"url":"https://github.com/acme/widgets/issues/8#c12",
             "body":"<!-- tally:spec-build:diagnosis:v1 -->\nmachine receipt",
             "createdAt":"2026-08-01T10:01:00Z","updatedAt":"2026-08-01T10:01:00Z",
             "author":{"login":"operator"}},
            {"databaseId":13,"url":"https://github.com/acme/widgets/issues/8#c13",
             "body":"drive-by opinion",
             "createdAt":"2026-08-01T10:02:00Z","updatedAt":"2026-08-01T10:02:00Z",
             "author":{"login":"stranger"}}
          ]}}]}}}}}"#;

    /// A walk the forge served in full, whose comment bodies happen to quote a
    /// GraphQL schema error. Comment bodies are writable by any account on a
    /// public repository — and by the campaign's own agents through the machine
    /// receipts tally posts to task threads — so nothing in here may reach the
    /// capability decision.
    const HOSTILE_WALK_PAYLOAD: &str = r#"{"data":{"repository":{"issue":{"subIssues":{
        "pageInfo":{"hasNextPage":false,"endCursor":null},
        "nodes":[{"number":8,
          "closedByPullRequestsReferences":{"nodes":[]},
          "comments":{"pageInfo":{"hasPreviousPage":true},"nodes":[
            {"databaseId":11,"url":"https://github.com/acme/widgets/issues/8#c11",
             "body":"CI says: Field 'foo' doesn't exist on type 'Bar' -- please fix",
             "createdAt":"2026-08-01T10:00:00Z","updatedAt":"2026-08-01T10:00:00Z",
             "author":{"login":"operator"}},
            {"databaseId":12,"url":"https://github.com/acme/widgets/issues/8#c12",
             "body":"see issue #334, which quotes UNDEFINED_FIELD verbatim",
             "createdAt":"2026-08-01T10:01:00Z","updatedAt":"2026-08-01T10:01:00Z",
             "author":{"login":"stranger"}}
          ]}}]}}}}}"#;

    #[test]
    fn the_arm_probe_answers_degraded_instead_of_failing_the_campaign() {
        let temporary = tempfile::tempdir().unwrap();
        let locator = parse_issue_url("https://github.com/acme/widgets/issues/42").unwrap();
        let gh_program = GhProgramGuard::acquire();

        let refusing = fake_gh(
            temporary.path(),
            "gh-refusing",
            "echo '{\"errors\":[{\"type\":\"UNDEFINED_FIELD\",\
             \"message\":\"Field '\\''subIssues'\\'' doesn'\\''t exist on type '\\''Issue'\\''\"}]}'; \
             echo \"gh: Field 'subIssues' doesn't exist on type 'Issue'\" >&2; exit 1",
        );
        gh_program.use_program(&refusing);
        // A forge whose schema has no such field is a capability answer, not an
        // error: the campaign still arms, in degraded mode.
        assert!(!probe_sub_issue_walk(&locator).unwrap());

        // A transport error, a rate limit, or a 502 says nothing about the
        // schema. Reading one as "this forge has no sub-issues" armed the
        // campaign degraded for the rest of its life over one bad minute, so
        // the probe now fails the arm and says why.
        let flaky = fake_gh(
            temporary.path(),
            "gh-flaky",
            "echo 'gh: HTTP 502: Bad gateway (https://api.github.com/graphql)' >&2; exit 1",
        );
        gh_program.use_program(&flaky);
        let failure = probe_sub_issue_walk(&locator).unwrap_err().to_string();
        assert!(failure.contains("capability probe failed"), "{failure}");
        assert!(failure.contains("502"), "{failure}");

        let serving = fake_gh(
            temporary.path(),
            "gh-serving",
            &format!("cat <<'TALLY_WALK'\n{WALK_PAYLOAD}\nTALLY_WALK"),
        );
        gh_program.use_program(&serving);
        assert!(probe_sub_issue_walk(&locator).unwrap());
        let walked = sub_issue_threads(&locator).unwrap();
        assert_eq!(walked.threads.keys().copied().collect::<Vec<_>>(), vec![8]);
        assert!(walked.truncated.is_empty());
        // The task thread carries human steering under the same contract the
        // master thread has always used: allowed actors only, machine
        // receipts excluded.
        let steering = task_steering(&locator, &["operator".to_owned()], &[8]).unwrap();
        assert_eq!(steering["8"].len(), 1);
        assert_eq!(steering["8"][0]["id"], json!(11));
        assert_eq!(steering["8"][0]["author"], json!("operator"));
    }

    /// A capability gate must not be answerable by an input a stranger writes.
    ///
    /// The first fix read the whole probe response for four phrases before it
    /// checked whether the call had even failed, and the response carries every
    /// comment body on every task thread. Quoting an ordinary CI error — or
    /// quoting issue #334, which contains the literal `UNDEFINED_FIELD` — was
    /// enough to answer "this forge has no sub-issue API", and the gate fails
    /// open into degraded mode for the life of the arm.
    #[test]
    fn a_served_walk_is_never_a_capability_refusal_whatever_its_comments_say() {
        let temporary = tempfile::tempdir().unwrap();
        let locator = parse_issue_url("https://github.com/acme/widgets/issues/42").unwrap();
        let gh_program = GhProgramGuard::acquire();

        let hostile = fake_gh(
            temporary.path(),
            "gh-hostile",
            &format!("cat <<'TALLY_WALK'\n{HOSTILE_WALK_PAYLOAD}\nTALLY_WALK"),
        );
        gh_program.use_program(&hostile);
        // The same payload walks successfully, which is the whole point: the
        // forge served the field, so the answer is native.
        let walked = sub_issue_threads(&locator).unwrap();
        assert_eq!(walked.threads.keys().copied().collect::<Vec<_>>(), vec![8]);
        assert!(
            probe_sub_issue_walk(&locator).unwrap(),
            "a served walk must arm native whatever its comment bodies quote"
        );

        // A genuine schema refusal still degrades even when GitHub types the
        // error only in the message, with no `type` or `extensions.code`.
        let untyped = fake_gh(
            temporary.path(),
            "gh-untyped-refusal",
            "echo '{\"errors\":[{\"message\":\"Field '\\''subIssues'\\'' doesn'\\''t exist on type '\\''Issue'\\''\"}]}'; exit 1",
        );
        gh_program.use_program(&untyped);
        assert!(!probe_sub_issue_walk(&locator).unwrap());

        // And a failure whose *body* merely quotes the phrase, with no such
        // message of its own, is a failure — not an answer.
        let quoting_failure = fake_gh(
            temporary.path(),
            "gh-quoting-failure",
            "echo '{\"data\":{\"note\":\"undefined_field\"}}';              echo 'gh: HTTP 502: Bad gateway' >&2; exit 1",
        );
        gh_program.use_program(&quoting_failure);
        let failure = probe_sub_issue_walk(&locator).unwrap_err().to_string();
        assert!(failure.contains("capability probe failed"), "{failure}");
    }

    /// The steering read's own comment window, on the surface that produces the
    /// steering an agent is briefed with. `last:` returns the newest, so an
    /// exhausted window drops the oldest approved comment silently.
    #[test]
    fn a_truncated_steering_window_is_reported_by_the_walk_that_reads_it() {
        let temporary = tempfile::tempdir().unwrap();
        let locator = parse_issue_url("https://github.com/acme/widgets/issues/42").unwrap();
        let gh_program = GhProgramGuard::acquire();

        let truncated = fake_gh(
            temporary.path(),
            "gh-truncated",
            &format!("cat <<'TALLY_WALK'\n{HOSTILE_WALK_PAYLOAD}\nTALLY_WALK"),
        );
        gh_program.use_program(&truncated);
        let walked = sub_issue_threads(&locator).unwrap();
        assert_eq!(
            walked.truncated.iter().copied().collect::<Vec<_>>(),
            vec![8]
        );
        // Reported, never refused: the walk still returns its threads and the
        // steering it did read.
        assert_eq!(walked.threads.keys().copied().collect::<Vec<_>>(), vec![8]);
        let steering = task_steering(&locator, &["operator".to_owned()], &[8]).unwrap();
        assert_eq!(steering["8"].len(), 1);

        let untruncated = fake_gh(
            temporary.path(),
            "gh-untruncated",
            &format!("cat <<'TALLY_WALK'\n{WALK_PAYLOAD}\nTALLY_WALK"),
        );
        gh_program.use_program(&untruncated);
        assert!(sub_issue_threads(&locator).unwrap().truncated.is_empty());
    }

    #[test]
    fn every_arm_path_says_which_projection_it_recorded() {
        // The enqueueing path prints the daemon's admission result, and a
        // campaign that armed degraded is otherwise indistinguishable from one
        // that armed native until an operator's sub-issue comment silently
        // fails to reach its agent.
        let admitted = json!({"task_uuid": "0198f000-0000-7000-8000-000000000002"});
        let native = armed_projection(&admitted, true);
        assert_eq!(native["projection"], json!("native-sub-issues"));
        assert_eq!(native["subIssueWalk"], json!(true));
        assert_eq!(native["task_uuid"], admitted["task_uuid"]);
        let degraded = armed_projection(&admitted, false);
        assert_eq!(degraded["projection"], json!("degraded-checkboxes"));
        assert_eq!(degraded["subIssueWalk"], json!(false));
        // A non-object admission result keeps its own shape rather than being
        // silently dropped to make room for the annotation.
        let wrapped = armed_projection(&json!("queued"), true);
        assert_eq!(wrapped["result"], json!("queued"));
        assert_eq!(wrapped["projection"], json!("native-sub-issues"));
    }

    #[test]
    fn resume_posts_one_auditable_counter_boundary() {
        let temporary = tempfile::tempdir().unwrap();
        let captured = temporary.path().join("resume-body");
        let fake = fake_gh(
            temporary.path(),
            "gh-resume",
            &format!(
                "cat > '{}'; printf '%s\\n' 'https://github.com/acme/widgets/issues/42#issuecomment-99'",
                captured.display()
            ),
        );
        let gh_program = GhProgramGuard::acquire();
        gh_program.use_program(&fake);

        let receipt = post_campaign_resume(
            &graph_for_forge_observation(),
            "operator",
            "  Corrected the unavailable external dependency.  ",
        )
        .unwrap();
        assert_eq!(
            receipt,
            "https://github.com/acme/widgets/issues/42#issuecomment-99"
        );
        let body = fs::read_to_string(captured).unwrap();
        assert!(
            body.lines().next().unwrap().starts_with(
                "<!-- tally:spec-build:resume:v1 campaign=night-build issue=42 nonce="
            ),
            "{body}"
        );
        assert!(body.contains("\n\n### Campaign resumed\n\n"), "{body}");
        assert!(
            body.contains("\n\nReason: Corrected the unavailable external dependency.\n\n"),
            "{body}"
        );
        assert!(body.ends_with("Requested by `@operator`."), "{body}");
        assert!(post_campaign_resume(&graph_for_forge_observation(), "operator", "   ").is_err());
    }

    #[test]
    fn amendment_pardons_only_escalated_tasks_that_gained_a_dependency() {
        let prior = canonical_graph_for_pardon(&[]);
        let amended = canonical_graph_for_pardon(&["prerequisite"]);
        let escalated = BTreeSet::from(["task-a".to_owned(), "task-b".to_owned()]);

        let (pardons, warnings) = amendment_pardon_plan(Some(&prior), &amended, &escalated);
        assert_eq!(
            pardons,
            [PlannedAutoPardon {
                task_id: "task-a".to_owned(),
                added_dependencies: vec!["prerequisite".to_owned()],
            }]
        );
        assert_eq!(
            warnings,
            ["task task-b remains escalated; run tally campaign resume to unblock"]
        );

        let receipt = arm_receipt(
            &json!({"status": "armed"}),
            true,
            &[AutoPardonReceipt {
                task_id: pardons[0].task_id.clone(),
                added_dependencies: pardons[0].added_dependencies.clone(),
                resume_receipt: "https://github.com/acme/widgets/issues/42#issuecomment-100"
                    .to_owned(),
            }],
            &warnings,
        );
        assert_eq!(receipt["autoPardons"][0]["taskId"], json!("task-a"));
        assert_eq!(
            receipt["autoPardons"][0]["addedDependencies"],
            json!(["prerequisite"])
        );
        assert_eq!(
            receipt["autoPardons"][0]["resumeReceipt"],
            json!("https://github.com/acme/widgets/issues/42#issuecomment-100")
        );
        assert_eq!(receipt["warnings"], json!(warnings));
    }

    #[test]
    fn auto_pardon_uses_the_resume_audit_receipt_with_a_task_scope() {
        let temporary = tempfile::tempdir().unwrap();
        let captured = temporary.path().join("auto-pardon-body");
        let fake = fake_gh(
            temporary.path(),
            "gh-auto-pardon",
            &format!(
                "cat > '{}'; printf '%s\\n' 'https://github.com/acme/widgets/issues/42#issuecomment-100'",
                captured.display()
            ),
        );
        let gh_program = GhProgramGuard::acquire();
        gh_program.use_program(&fake);
        let pardon = PlannedAutoPardon {
            task_id: "foundation".to_owned(),
            added_dependencies: vec!["prerequisite".to_owned()],
        };

        let receipt =
            post_campaign_auto_pardon(&graph_for_forge_observation(), "operator", &pardon).unwrap();
        assert!(receipt.ends_with("#issuecomment-100"));
        let body = fs::read_to_string(captured).unwrap();
        let marker = body.lines().next().unwrap();
        assert!(
            marker.starts_with(
                "<!-- tally:spec-build:resume:v1 campaign=night-build issue=42 nonce="
            ),
            "{body}"
        );
        assert!(marker.ends_with(" tasks=foundation -->"), "{body}");
        assert!(body.contains("\n\n### Campaign resumed\n\n"), "{body}");
        assert!(
            body.contains("receipts for `foundation` without deleting the audit trail."),
            "{body}"
        );
        assert!(
            body.contains("Reason: Re-armed graph added dependency `prerequisite`"),
            "{body}"
        );
        assert!(body.ends_with("Requested by `@operator`."), "{body}");
    }

    #[test]
    fn active_escalation_detection_honors_scoped_resume_boundaries() {
        let temporary = tempfile::tempdir().unwrap();
        let comments_path = temporary.path().join("comments.json");
        let diagnosis = |id: u64, attempt: u8| {
            json!({
                "id": id,
                "body": format!(
                    "<!-- tally:spec-build:diagnosis:v1 campaign=night-build issue=42 task=foundation attempt={attempt} -->\n\nreceipt"
                ),
                "html_url": format!("https://github.com/acme/widgets/issues/42#issuecomment-{id}"),
                "created_at": "2026-08-10T10:00:00Z",
                "updated_at": "2026-08-10T10:00:00Z",
                "user": {"login": "operator"},
            })
        };
        let mut comments = vec![
            diagnosis(10, 1),
            diagnosis(11, 2),
            json!({
                "id": 12,
                "body": "<!-- tally:spec-build:escalation:v1 campaign=night-build issue=42 -->\n\n### Spec-build escalation: frontier quiescent",
                "html_url": "https://github.com/acme/widgets/issues/42#issuecomment-12",
                "created_at": "2026-08-10T10:01:00Z",
                "updated_at": "2026-08-10T10:01:00Z",
                "user": {"login": "operator"},
            }),
        ];
        fs::write(
            &comments_path,
            serde_json::to_vec(&json!([comments])).unwrap(),
        )
        .unwrap();
        let fake = fake_gh(
            temporary.path(),
            "gh-active-escalation",
            &format!(
                r#"case "$*" in
  "api --paginate --slurp repos/acme/widgets/issues/42/comments?per_page=100") cat '{}' ;;
  *) echo "unexpected gh call: $*" >&2; exit 97 ;;
esac"#,
                comments_path.display()
            ),
        );
        let gh_program = GhProgramGuard::acquire();
        gh_program.use_program(&fake);
        let graph = graph_for_forge_observation();
        assert_eq!(
            active_escalated_tasks(&graph, "operator", false).unwrap(),
            BTreeSet::from(["foundation".to_owned()])
        );

        comments.push(json!({
            "id": 13,
            "body": "<!-- tally:spec-build:resume:v1 campaign=night-build issue=42 nonce=018f47a0-7b9d-7cc2-92d6-2f7f19f505fd tasks=foundation -->\n\n### Campaign resumed\n\nPardoned prior receipts.\n\nReason: The amendment added a dependency.",
            "html_url": "https://github.com/acme/widgets/issues/42#issuecomment-13",
            "created_at": "2026-08-10T10:02:00Z",
            "updated_at": "2026-08-10T10:02:00Z",
            "user": {"login": "operator"},
        }));
        fs::write(
            &comments_path,
            serde_json::to_vec(&json!([comments])).unwrap(),
        )
        .unwrap();
        assert!(
            active_escalated_tasks(&graph, "operator", false)
                .unwrap()
                .is_empty(),
            "the task-scoped resume marker must pardon only that task's active generation"
        );
    }

    #[test]
    fn a_projection_written_before_the_probe_has_no_capability_answer() {
        let projection: CampaignProjectionV1 = serde_json::from_value(json!({
            "schemaVersion": CAMPAIGN_PROJECTION_SCHEMA_VERSION,
            "codeRepository": "acme/widgets",
            "worklistPattern": "specs/night/tasks.json",
            "sourceRevision": "a".repeat(40),
            "worklistSha256": format!("sha256:{}", "b".repeat(64)),
            "issue": {
                "repository": "acme/widgets",
                "number": 42,
                "url": "https://github.com/acme/widgets/issues/42"
            }
        }))
        .unwrap();
        assert_eq!(projection.sub_issue_walk, None);
    }

    #[test]
    fn a_local_projection_need_not_invent_a_forge_issue() {
        let temporary = tempfile::tempdir().unwrap();
        let projection = CampaignProjectionV1 {
            schema_version: CAMPAIGN_PROJECTION_SCHEMA_VERSION,
            code_repository: "acme/widgets".to_owned(),
            worklist_pattern: "specs/night/tasks.json".to_owned(),
            source_revision: "a".repeat(40),
            worklist_sha256: format!("sha256:{}", "b".repeat(64)),
            issue: None,
            sub_issue_walk: None,
        };
        write_campaign_projection(temporary.path(), &projection).unwrap();

        let loaded = read_campaign_projection(
            temporary.path(),
            &projection.code_repository,
            &projection.worklist_pattern,
        )
        .unwrap();
        assert!(loaded.issue.is_none());
        assert!(loaded.locator().is_err());
    }

    #[test]
    fn a_task_thread_comment_moves_the_observation_revision() {
        let graph = CampaignGraph {
            locator: parse_issue_url("https://github.com/acme/widgets/issues/42").unwrap(),
            canonical: CanonicalCampaignGraphV1 {
                manifest: serde_json::from_value(manifest_value_for_test(json!([{
                    "id": "foundation",
                    "kind": "implementation",
                    "issue": 43,
                    "dependencies": [],
                    "conflictDomains": []
                }])))
                .unwrap(),
                tasks: Vec::new(),
                executable_digest: format!("sha256:{}", "a".repeat(64)),
            },
            master: GithubIssue {
                number: 42,
                title: "Campaign".to_owned(),
                body: None,
                state: "open".to_owned(),
                html_url: "https://github.com/acme/widgets/issues/42".to_owned(),
                updated_at: "2026-08-01T10:00:00Z".to_owned(),
                user: GithubActor {
                    login: "operator".to_owned(),
                },
                pull_request: None,
            },
            tasks: Vec::new(),
        };
        let quiet = CampaignSteering::default();
        let steered = CampaignSteering {
            master: Vec::new(),
            tasks: BTreeMap::from([("43".to_owned(), vec![json!({"body": "rerun it"})])]),
        };
        let repository_progress = json!({"base": "a"});
        assert_ne!(
            campaign_observation(&graph, &quiet, &repository_progress, 1).unwrap(),
            campaign_observation(&graph, &steered, &repository_progress, 1).unwrap()
        );
    }

    #[test]
    fn campaign_completion_and_summary_comments_are_machine_state_not_steering() {
        for body in [
            "<!-- tally:spec-build:diagnosis:v1 campaign=x -->\nreceipt",
            "<!-- tally:campaign-complete:v1 source=sha256:abc -->\nsummary",
            "<!-- tally:campaign-summary:v1 campaign=x outcome=blocked -->\nsummary",
        ] {
            assert!(tally_authored_comment(body), "{body:?}");
        }
        assert!(!tally_authored_comment("please rerun the failed task"));
    }

    #[test]
    fn timestamp_only_machine_writes_wake_the_poll_without_moving_the_campaign() {
        let base = graph_for_forge_observation();
        let mut machine_touched = graph_for_forge_observation();
        machine_touched.master.updated_at = "2026-08-01T11:00:00Z".to_owned();
        machine_touched.tasks[0].updated_at = "2026-08-01T11:00:00Z".to_owned();
        let repository_progress = json!({"base": "a", "campaignRefs": {}});
        let steering = CampaignSteering::default();

        assert_ne!(
            forge_observation(&base, &repository_progress, 1).unwrap(),
            forge_observation(&machine_touched, &repository_progress, 1).unwrap(),
            "the cheap precondition must still notice that comments may need rereading"
        );
        assert_eq!(
            campaign_observation(&base, &steering, &repository_progress, 1).unwrap(),
            campaign_observation(&machine_touched, &steering, &repository_progress, 1).unwrap(),
            "filtered machine writes must not enqueue a new reconcile pass"
        );
    }

    #[test]
    fn a_graph_mismatch_requires_the_same_complete_forge_snapshot_twice() {
        let graph = graph_for_forge_observation();
        let first = graph_mismatch_observation(&graph, 1).unwrap();
        assert_eq!(graph_mismatch_observation(&graph, 1).unwrap(), first);

        let mut moving = graph;
        moving.tasks[0].updated_at = "2026-08-01T11:00:00Z".to_owned();
        assert_ne!(graph_mismatch_observation(&moving, 1).unwrap(), first);
    }

    /// The poll skips the expensive sub-issue walk while this digest holds
    /// still, so anything the walk could see has to move it.
    #[test]
    fn the_cheap_poll_precondition_moves_with_every_surface_the_walk_reads() {
        let base = graph_for_forge_observation();
        let unchanged = graph_for_forge_observation();
        let repository_progress = json!({"base": "a", "campaignRefs": {}});
        assert_eq!(
            forge_observation(&base, &repository_progress, 1).unwrap(),
            forge_observation(&unchanged, &repository_progress, 1).unwrap()
        );
        let quiet = forge_observation(&base, &repository_progress, 1).unwrap();

        // A comment on the master thread bumps the master's updated_at.
        let mut master_touched = graph_for_forge_observation();
        master_touched.master.updated_at = "2026-08-01T11:00:00Z".to_owned();
        assert_ne!(
            forge_observation(&master_touched, &repository_progress, 1).unwrap(),
            quiet
        );

        // A comment on a task's own sub-issue bumps that sub-issue's.
        let mut task_touched = graph_for_forge_observation();
        task_touched.tasks[0].updated_at = "2026-08-01T11:00:00Z".to_owned();
        assert_ne!(
            forge_observation(&task_touched, &repository_progress, 1).unwrap(),
            quiet
        );

        // A merged pull request closing a sub-issue changes its state.
        let mut task_closed = graph_for_forge_observation();
        task_closed.tasks[0].state = "closed".to_owned();
        assert_ne!(
            forge_observation(&task_closed, &repository_progress, 1).unwrap(),
            quiet
        );

        // A local merge or checkpoint moves durable Git state even when every
        // issue and comment remains byte-for-byte unchanged.
        let advanced_repository = json!({"base": "b", "campaignRefs": {}});
        assert_ne!(
            forge_observation(&base, &advanced_repository, 1).unwrap(),
            quiet
        );

        // Re-arming always dispatches, so it must invalidate the precondition.
        assert_ne!(
            forge_observation(&base, &repository_progress, 2).unwrap(),
            quiet
        );
    }

    #[test]
    fn repository_progress_tracks_the_driver_base_and_scoped_refs() {
        assert_eq!(
            campaign_state_ref_prefix("final-bar", 7),
            "refs/tally/spec-build/v1/049836c3e38c7ecc9c638e9c"
        );

        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        let remote = temporary.path().join("remote.git");
        fs::create_dir(&checkout).unwrap();
        let bare = ProcessCommand::new("git")
            .args(["init", "--bare", "--quiet", "--initial-branch=main"])
            .arg(&remote)
            .status()
            .unwrap();
        assert!(bare.success());
        let git = |arguments: &[&str]| -> String {
            let output = ProcessCommand::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(arguments)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {:?} failed: {}",
                arguments,
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap()
        };
        git(&["init", "--quiet", "--initial-branch=main"]);
        git(&["config", "user.name", "Campaign Test"]);
        git(&["config", "user.email", "campaign@example.invalid"]);
        fs::write(checkout.join("README.md"), "base\n").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "--quiet", "-m", "base"]);
        git(&["remote", "add", "origin", remote.to_str().unwrap()]);
        git(&["push", "--quiet", "--set-upstream", "origin", "main"]);

        let mut graph = graph_for_forge_observation();
        graph.canonical.manifest.repository.checkout = checkout.clone();
        graph.canonical.manifest.repository.forge = "local".to_owned();
        let initial = repository_progress_value(&graph).unwrap();

        fs::write(checkout.join("README.md"), "base\nmerged\n").unwrap();
        git(&["add", "README.md"]);
        git(&["commit", "--quiet", "-m", "merge"]);
        git(&["push", "--quiet", "origin", "main"]);
        let merged = repository_progress_value(&graph).unwrap();
        assert_ne!(merged, initial, "a local base merge must wake a plain poll");

        fs::write(checkout.join("checkpoint.json"), "{}\n").unwrap();
        let object = git(&["hash-object", "-w", "checkpoint.json"])
            .trim()
            .to_owned();
        let reference = format!(
            "{}/checkpoint/gate",
            campaign_state_ref_prefix(&graph.canonical.manifest.name, graph.locator.number,)
        );
        let refspec = format!("{object}:{reference}");
        git(&["push", "--quiet", "origin", &refspec]);
        let checkpointed = repository_progress_value(&graph).unwrap();
        assert_ne!(
            checkpointed, merged,
            "a campaign-scoped checkpoint must wake a plain poll"
        );
    }

    fn graph_for_forge_observation() -> CampaignGraph {
        let task = GithubIssue {
            number: 43,
            title: "Foundation".to_owned(),
            body: None,
            state: "open".to_owned(),
            html_url: "https://github.com/acme/widgets/issues/43".to_owned(),
            updated_at: "2026-08-01T10:00:00Z".to_owned(),
            user: GithubActor {
                login: "operator".to_owned(),
            },
            pull_request: None,
        };
        CampaignGraph {
            locator: parse_issue_url("https://github.com/acme/widgets/issues/42").unwrap(),
            canonical: CanonicalCampaignGraphV1 {
                manifest: serde_json::from_value(manifest_value_for_test(json!([{
                    "id": "foundation",
                    "kind": "implementation",
                    "issue": 43,
                    "dependencies": [],
                    "conflictDomains": []
                }])))
                .unwrap(),
                tasks: vec![CanonicalCampaignTaskV1 {
                    number: task.number,
                    title: task.title.clone(),
                    body: task.body.clone().unwrap_or_default(),
                }],
                executable_digest: format!("sha256:{}", "a".repeat(64)),
            },
            master: GithubIssue {
                number: 42,
                title: "Campaign".to_owned(),
                body: None,
                state: "open".to_owned(),
                html_url: "https://github.com/acme/widgets/issues/42".to_owned(),
                updated_at: "2026-08-01T10:00:00Z".to_owned(),
                user: GithubActor {
                    login: "operator".to_owned(),
                },
                pull_request: None,
            },
            tasks: vec![task],
        }
    }

    fn canonical_graph_for_pardon(task_a_dependencies: &[&str]) -> CanonicalCampaignGraphV1 {
        let manifest: CampaignManifest = serde_json::from_value(manifest_value_for_test(json!([
            {
                "id": "prerequisite",
                "kind": "implementation",
                "issue": 43,
                "dependencies": [],
                "conflictDomains": []
            },
            {
                "id": "task-a",
                "kind": "implementation",
                "issue": 44,
                "dependencies": task_a_dependencies,
                "conflictDomains": []
            },
            {
                "id": "task-b",
                "kind": "implementation",
                "issue": 45,
                "dependencies": [],
                "conflictDomains": []
            }
        ])))
        .unwrap();
        CanonicalCampaignGraphV1::new(
            manifest,
            vec![
                CanonicalCampaignTaskV1 {
                    number: 43,
                    title: "Prerequisite".to_owned(),
                    body: "Prepare the dependency.".to_owned(),
                },
                CanonicalCampaignTaskV1 {
                    number: 44,
                    title: "Task A".to_owned(),
                    body: "Implement task A.".to_owned(),
                },
                CanonicalCampaignTaskV1 {
                    number: 45,
                    title: "Task B".to_owned(),
                    body: "Implement task B.".to_owned(),
                },
            ],
        )
        .unwrap()
    }

    #[test]
    fn issue_url_is_canonical_and_bounded() {
        let locator = parse_issue_url("https://github.com/acme/widgets/issues/42").unwrap();
        assert_eq!(locator.repository, "acme/widgets");
        assert_eq!(locator.number, 42);
        assert!(parse_issue_url("http://github.com/acme/widgets/issues/42").is_err());
        assert!(parse_issue_url("https://github.com/acme/widgets/pull/42").is_err());
        assert!(parse_issue_url("https://github.com/acme/widgets/issues/42?x=1").is_err());
    }

    #[test]
    fn arm_manifest_defaults_to_the_repository_campaign_namespace() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let status = ProcessCommand::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());

        let mut value = manifest_value_for_test(json!([{
            "id": "foundation",
            "kind": "implementation",
            "issue": 43,
            "dependencies": [],
            "conflictDomains": []
        }]));
        value["repository"]["checkout"] = json!(checkout);
        value["repository"]["forge"] = json!("local");
        let body = |manifest: &Value| {
            format!(
                "{CAMPAIGN_BEGIN}\n```json\n{}\n```\n{CAMPAIGN_END}",
                serde_json::to_string(manifest).unwrap()
            )
        };

        let admitted = parse_manifest(&body(&value), "acme/widgets").unwrap();
        assert_eq!(admitted.pool, "campaign/acme/widgets");

        value["pool"] = json!("legacy-runner");
        let explicit = parse_manifest(&body(&value), "acme/widgets").unwrap();
        assert_eq!(explicit.pool, "legacy-runner");
    }

    #[test]
    fn arm_manifest_validates_campaign_namespace_shape_and_repository() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let status = ProcessCommand::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());

        let mut value = manifest_value_for_test(json!([{
            "id": "foundation",
            "kind": "implementation",
            "issue": 43,
            "dependencies": [],
            "conflictDomains": []
        }]));
        value["repository"]["checkout"] = json!(checkout);
        value["repository"]["forge"] = json!("local");
        let parse = |value: &Value| {
            let body = format!(
                "{CAMPAIGN_BEGIN}\n```json\n{}\n```\n{CAMPAIGN_END}",
                serde_json::to_string(value).unwrap()
            );
            parse_manifest(&body, "acme/widgets")
        };

        value["pool"] = json!("campaign/acme/widgets");
        assert_eq!(parse(&value).unwrap().pool, "campaign/acme/widgets");
        for invalid in [
            "campaign/acme",
            "campaign//widgets",
            "campaign/acme/widgets/extra",
        ] {
            value["pool"] = json!(invalid);
            let error = parse(&value).unwrap_err().to_string();
            assert!(error.contains("campaign/OWNER/REPO"), "{error}");
        }
        value["pool"] = json!("campaign/acme/other");
        let error = parse(&value).unwrap_err().to_string();
        assert!(error.contains("must match issue repository"), "{error}");
    }

    #[test]
    fn namespace_runner_bypasses_config_while_explicit_runner_keeps_mutex_validation() {
        let mut pools = BTreeMap::new();
        validate_campaign_runner_pool("campaign/acme/widgets", &pools).unwrap();
        let missing = validate_campaign_runner_pool("legacy-runner", &pools)
            .unwrap_err()
            .to_string();
        assert!(missing.contains("require configured pool"), "{missing}");

        pools.insert(
            "legacy-runner".to_owned(),
            PoolConfig {
                resource: Some(ResourceKind::Mutex),
                capacity: 1,
                ..PoolConfig::default()
            },
        );
        validate_campaign_runner_pool("legacy-runner", &pools).unwrap();
        pools.get_mut("legacy-runner").unwrap().capacity = 2;
        let invalid = validate_campaign_runner_pool("legacy-runner", &pools)
            .unwrap_err()
            .to_string();
        assert!(invalid.contains("capacity-1 mutex"), "{invalid}");
    }

    #[test]
    fn managed_sections_preserve_operator_prose() {
        let body = "Operator context.\n\n<!-- tally:campaign:v1 -->\nold\n<!-- tally:campaign:v1:end -->\n\nTail.\n";
        let updated =
            upsert_managed_section(body, CAMPAIGN_BEGIN, CAMPAIGN_END, "\nnew\n").unwrap();
        assert!(updated.starts_with("Operator context."));
        assert!(updated.ends_with("\n\nTail.\n"));
        assert!(updated.contains("\nnew\n"));
        assert!(!updated.contains("old"));
        assert!(extract_managed_section(
            "<!-- tally:campaign:v1 -->x<!-- tally:campaign:v1:end --><!-- tally:campaign:v1:end -->",
            CAMPAIGN_BEGIN,
            CAMPAIGN_END,
        )
        .is_err());
    }

    #[test]
    fn project_worklist_accepts_exact_issue_briefs() {
        let document = json!({
            "schemaVersion": 1,
            "tasks": [
                {"id": "foundation", "kind": "implementation", "title": "Foundation", "body": "Do the first thing.", "dependencies": [], "conflictDomains": ["src"]},
                {"id": "finish", "kind": "implementation", "title": "Finish", "body": "Do the next thing.", "dependencies": ["foundation"], "conflictDomains": ["tests"]}
            ]
        });
        let tasks = project_tasks(&document, "night-build").unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].dependencies, ["foundation"]);
    }

    #[test]
    fn project_synthesizes_checkpoint_brief_from_fixture_worklist() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/checkpoint-bare-worklist.json");
        let document: Value = serde_json::from_str(
            &fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display())),
        )
        .expect("checkpoint worklist fixture must be JSON");
        let mut config = project_config(&document, None).unwrap();
        let campaign_name = config["name"].as_str().unwrap();
        let tasks = project_tasks(&document, campaign_name).unwrap();
        let checkpoint = tasks
            .iter()
            .find(|task| task.kind == "checkpoint")
            .expect("fixture must contain a checkpoint");
        assert_eq!(
            checkpoint.body,
            "## Checkpoint\n\nCampaign: `fixture-checkpoint-render`\n\n## Gate argv\n\n    [\"bash\",\"test/fleet-gate.sh\"]\n\n## Runtime limit\n\n7200 seconds\n\n## Dependencies\n\n    [\"prepare\"]\n"
        );

        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let status = ProcessCommand::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());
        config["repository"]["checkout"] = json!(fs::canonicalize(checkout).unwrap());
        let manifest = validate_project_shape(&config, &tasks).unwrap();
        assert_eq!(manifest.tasks.len(), 2);
        assert_eq!(manifest.tasks[1].kind, "checkpoint");
    }

    #[test]
    fn committed_silent_factory_worklists_render_checkpoint_briefs() {
        let directory =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../silent-factory-worklists");
        let mut paths = fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
            .collect::<Vec<_>>();
        paths.sort();
        assert!(!paths.is_empty(), "silent-factory worklists must exist");

        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let status = ProcessCommand::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());
        let checkout = fs::canonicalize(checkout).unwrap();

        for path in paths {
            let document: Value = serde_json::from_str(
                &fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display())),
            )
            .unwrap_or_else(|error| panic!("{} is not JSON: {error}", path.display()));
            let stem = path.file_stem().and_then(|value| value.to_str()).unwrap();
            let campaign_name = format!("silent-factory-{stem}");
            let tasks = project_tasks(&document, &campaign_name)
                .unwrap_or_else(|error| panic!("cannot render {}: {error}", path.display()));
            let checkpoints = tasks
                .iter()
                .filter(|task| task.kind == "checkpoint")
                .collect::<Vec<_>>();
            assert_eq!(
                checkpoints.len(),
                1,
                "{} must contain its chapter checkpoint",
                path.display()
            );
            for checkpoint in checkpoints {
                let argv = serde_json::to_string(checkpoint.argv.as_ref().unwrap()).unwrap();
                let dependencies = serde_json::to_string(&checkpoint.dependencies).unwrap();
                assert!(
                    checkpoint
                        .body
                        .contains(&format!("Campaign: `{campaign_name}`")),
                    "{} omits its campaign name",
                    path.display()
                );
                assert!(
                    checkpoint.body.contains(&format!("    {argv}")),
                    "{} omits its checkpoint argv",
                    path.display()
                );
                assert!(
                    checkpoint
                        .body
                        .contains(&format!("{} seconds", checkpoint.runtime_max_sec.unwrap())),
                    "{} omits its checkpoint runtime",
                    path.display()
                );
                assert!(
                    checkpoint.body.contains(&format!("    {dependencies}")),
                    "{} omits its checkpoint dependencies",
                    path.display()
                );
            }

            let mut config = manifest_value_for_test(json!([]));
            config["name"] = json!(campaign_name);
            config["repository"]["checkout"] = json!(checkout);
            config["repository"]["forge"] = json!("local");
            config["maxTasks"] = json!(100);
            config.as_object_mut().unwrap().remove("tasks");
            validate_project_shape(&config, &tasks)
                .unwrap_or_else(|error| panic!("cannot project {}: {error}", path.display()));
        }
    }

    #[test]
    fn project_and_admission_preserve_omitted_conflict_domains() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let status = ProcessCommand::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());

        let document = json!({
            "schemaVersion": 1,
            "tasks": [{
                "id": "serial",
                "kind": "implementation",
                "title": "Serial task",
                "body": "Make the bounded change.",
                "issue": 43,
                "dependencies": []
            }]
        });
        let tasks = project_tasks(&document, "night-build").unwrap();
        assert_eq!(tasks[0].conflict_domains, None);
        let references = task_references(&tasks).unwrap();
        assert!(!references[0]
            .as_object()
            .unwrap()
            .contains_key("conflictDomains"));

        let mut config = manifest_value_for_test(json!([]));
        config["repository"]["checkout"] = json!(checkout);
        let admitted = admit_manifest_value(manifest_value(&config, &tasks).unwrap()).unwrap();
        assert_eq!(admitted.tasks[0].conflict_domains, None);
        let canonical = serde_json::to_value(&admitted).unwrap();
        assert!(!canonical["tasks"][0]
            .as_object()
            .unwrap()
            .contains_key("conflictDomains"));

        let explicit = json!({
            "schemaVersion": 1,
            "tasks": [{
                "id": "serial",
                "kind": "implementation",
                "title": "Serial task",
                "body": "Make no change.",
                "issue": 43,
                "dependencies": [],
                "conflictDomains": []
            }]
        });
        let explicit_tasks = project_tasks(&explicit, "night-build").unwrap();
        assert_eq!(explicit_tasks[0].conflict_domains, Some(Vec::new()));
        assert_eq!(
            task_references(&explicit_tasks).unwrap()[0]["conflictDomains"],
            json!([])
        );

        config["maxParallel"] = json!(2);
        let error = admit_manifest_value(manifest_value(&config, &tasks).unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("conflictDomains: must be non-empty when campaign maxParallel"),
            "{error}"
        );
    }

    #[test]
    fn project_rejects_an_oversized_issue_brief_before_projection() {
        let document = json!({
            "schemaVersion": 1,
            "tasks": [{
                "id": "foundation",
                "kind": "implementation",
                "title": "Foundation",
                "body": "x".repeat(64_001),
                "dependencies": [],
                "conflictDomains": []
            }]
        });
        assert!(project_tasks(&document, "night-build").is_err());
    }

    #[test]
    fn worklist_projection_must_match_manifest_but_checkbox_state_is_not_truth() {
        let value = manifest_value_for_test(json!([{
            "id": "foundation",
            "kind": "implementation",
            "issue": 43,
            "dependencies": [],
            "conflictDomains": []
        }]));
        let manifest: CampaignManifest = serde_json::from_value(value.clone()).unwrap();
        let body = format!(
            "{CAMPAIGN_BEGIN}\n```json\n{}\n```\n{CAMPAIGN_END}\n{WORKLIST_BEGIN}\n- [X] {TASK_MARKER_PREFIX}foundation --> #43 — Foundation\n{WORKLIST_END}\n",
            serde_json::to_string_pretty(&value).unwrap()
        );
        validate_worklist_projection(&body, &manifest).unwrap();
        assert!(validate_worklist_projection(&body.replace("#43", "#44"), &manifest).is_err());
    }

    #[test]
    fn project_shape_reports_the_configured_max_tasks_overflow() {
        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        let status = ProcessCommand::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(&checkout)
            .status()
            .unwrap();
        assert!(status.success());

        let document = json!({
            "schemaVersion": 1,
            "tasks": [
                {"id": "one", "kind": "implementation", "title": "One", "body": "First.", "dependencies": [], "conflictDomains": []},
                {"id": "two", "kind": "implementation", "title": "Two", "body": "Second.", "dependencies": ["one"], "conflictDomains": []}
            ]
        });
        let mut config = manifest_value_for_test(json!([]));
        config["repository"]["checkout"] = json!(fs::canonicalize(&checkout).unwrap());
        config.as_object_mut().unwrap().remove("tasks");
        config
            .as_object_mut()
            .unwrap()
            .insert("maxTasks".into(), json!(1));
        let error =
            validate_project_shape(&config, &project_tasks(&document, "night-build").unwrap())
                .unwrap_err()
                .to_string();
        assert_eq!(
            error,
            "campaign configuration cannot form a valid manifest: campaign contains 2 tasks but manifest maxTasks is 1 — raise \"maxTasks\" in the campaign manifest"
        );
    }

    #[test]
    fn explicit_missing_asset_never_falls_back() {
        assert!(resolve_asset(
            Some(PathBuf::from("/definitely/missing/tally-campaign-flow")),
            "TALLY_TEST_UNUSED_ASSET",
            "missing-installed",
            "examples/flows/spec-build.js",
        )
        .is_err());
    }

    #[test]
    fn forge_native_continuation_re_enters_through_the_registry_scan() {
        let host = CampaignHost {
            socket: Path::new("/run/user/1000/tally/tally.sock"),
            config_path: Some(Path::new("/home/operator/.config/tally/config.json")),
            state_dir: Path::new("/home/operator/.local/state/tally"),
            rpc_timeout: Duration::from_secs(30),
        };
        assert_eq!(
            host.tally_argv_prefix(Path::new("/nix/store/tally/bin/tally")),
            vec![
                "/nix/store/tally/bin/tally",
                "--config",
                "/home/operator/.config/tally/config.json",
                "--socket",
                "/run/user/1000/tally/tally.sock",
            ]
        );
        assert_eq!(
            host.dispatch_flow_argv(
                Path::new("/nix/store/tally/bin/tally"),
                Path::new("/nix/store/spec-build.js"),
                16,
                None,
            ),
            vec![
                "/nix/store/tally/bin/tally",
                "--config",
                "/home/operator/.config/tally/config.json",
                "--socket",
                "/run/user/1000/tally/tally.sock",
                "flow",
                "run",
                "/nix/store/spec-build.js",
                "--args-from-brief",
                "--max-nodes",
                "16",
            ]
        );
        // Byte-for-byte the public poll an operator or timer runs. Durable Git
        // progress, not a private argument, gives the successor a fresh
        // observation and enqueue identity.
        assert_eq!(
            host.continuation_argv(Path::new("/nix/store/tally/bin/tally")),
            vec![
                "/nix/store/tally/bin/tally",
                "--config",
                "/home/operator/.config/tally/config.json",
                "--socket",
                "/run/user/1000/tally/tally.sock",
                "campaign",
                "poll",
                "--once",
                "--state-dir",
                "/home/operator/.local/state/tally",
            ]
        );
        assert_eq!(
            host.events_dir(),
            Path::new("/home/operator/.local/state/tally/events")
        );
        let without_config = CampaignHost {
            config_path: None,
            ..host
        };
        assert_eq!(
            without_config.tally_argv_prefix(Path::new("/nix/store/tally/bin/tally")),
            vec![
                "/nix/store/tally/bin/tally",
                "--socket",
                "/run/user/1000/tally/tally.sock",
            ],
            "an omitted config locator must not synthesize an XDG path or a \
             --config flag"
        );
        assert_eq!(
            without_config.continuation_argv(Path::new("/nix/store/tally/bin/tally")),
            vec![
                "/nix/store/tally/bin/tally",
                "--socket",
                "/run/user/1000/tally/tally.sock",
                "campaign",
                "poll",
                "--once",
                "--state-dir",
                "/home/operator/.local/state/tally",
            ]
        );
    }

    #[test]
    fn flow_node_bound_includes_pass_maintenance_and_cleanup() {
        let mut value = manifest_value_for_test(json!([]));
        let object = value.as_object_mut().unwrap();
        object.insert("maxParallel".into(), json!(3));
        object.insert(
            "gates".into(),
            json!([
                {
                    "kind": "command",
                    "id": "test",
                    "preflightArgv": ["true"],
                    "argv": ["true"]
                },
                {
                    "kind": "forbidPaths",
                    "id": "no-databases",
                    "forbidPaths": ["*.db"]
                }
            ]),
        );
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        // The Nix module computes this budget independently in
        // campaignMaxNodes. Its fixture campaign has this exact shape and is
        // asserted to be 55 too; change one side and the other must follow.
        // 3 + (2 + 2*1) + 3*(12 + 2*2) = 55.
        assert_eq!(max_flow_nodes(&manifest), 55);
    }

    #[test]
    fn flow_node_bound_covers_lanes_that_fail_at_merge() {
        // A lane that fails at merge spends every success-path node and then
        // its machinery retry, diff, diagnosis, and steering on top. maxNodes
        // counts cumulative rows, so the budget must hold all of them at once:
        // a machinery fault past its retry budget records the retry receipt
        // node and is steered in the same pass. On top of that, a pass before
        // the first merge also pays for the pristine-base preflight lane: its
        // prep and cleanup, plus a gating probe and a non-gating real-argv
        // witness for every command gate.
        const PASS_MAINTENANCE: usize = 3;
        const LANE_SUCCESS_PATH: usize = 8;
        const LANE_FAILURE_PATH: usize = 4;
        const PREFLIGHT_LANE: usize = 2;
        const PREFLIGHT_PER_COMMAND_GATE: usize = 2;

        for max_parallel in 1..=4 {
            for command_gates in 0..=2 {
                for constraint_gates in 0..=3 {
                    let mut value = manifest_value_for_test(json!([]));
                    let object = value.as_object_mut().unwrap();
                    object.insert("maxParallel".into(), json!(max_parallel));
                    object.insert(
                        "gates".into(),
                        Value::Array(
                            (0..command_gates)
                                .map(|index| {
                                    json!({
                                        "kind": "command",
                                        "id": format!("tests-{index}"),
                                        "preflightArgv": ["true"],
                                        "argv": ["true"]
                                    })
                                })
                                .chain((0..constraint_gates).map(|index| {
                                    json!({
                                        "kind": "forbidPaths",
                                        "id": format!("no-databases-{index}"),
                                        "forbidPaths": ["*.db"]
                                    })
                                }))
                                .collect(),
                        ),
                    );
                    let manifest: CampaignManifest = serde_json::from_value(value).unwrap();

                    let preflight = if command_gates == 0 {
                        0
                    } else {
                        PREFLIGHT_LANE + PREFLIGHT_PER_COMMAND_GATE * command_gates
                    };
                    let gate_count = command_gates + constraint_gates;
                    let worst_case = PASS_MAINTENANCE
                        + preflight
                        + max_parallel * (LANE_SUCCESS_PATH + LANE_FAILURE_PATH + 2 * gate_count);
                    assert!(
                        max_flow_nodes(&manifest) as usize >= worst_case,
                        "maxParallel {max_parallel} with {command_gates} command and \
                         {constraint_gates} constraint gates budgets {} nodes but a frontier \
                         failing at merge after a full preflight needs {worst_case}",
                        max_flow_nodes(&manifest)
                    );
                }
            }
        }
    }

    #[test]
    fn manifest_defaults_to_squash_with_no_steward_and_refuses_other_methods() {
        // The campaign default is squash on both sides of the seam: the Nix
        // module renders it into the brief and a forge-native manifest that
        // names nothing gets the same integration.
        let tasks = json!([{ "id": "task-1", "kind": "implementation", "issue": 8 }]);
        let manifest: CampaignManifest =
            serde_json::from_value(manifest_value_for_test(tasks.clone())).unwrap();
        assert_eq!(manifest.merge_method, "squash");
        assert!(manifest.steward.is_none());
        validate_manifest(&manifest).unwrap();

        let mut value = manifest_value_for_test(tasks.clone());
        value
            .as_object_mut()
            .unwrap()
            .insert("mergeMethod".into(), json!("rebase"));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("mergeMethod must be merge or squash"),
            "{error}"
        );

        let mut value = manifest_value_for_test(tasks);
        let object = value.as_object_mut().unwrap();
        object.insert("mergeMethod".into(), json!("merge"));
        object.insert(
            "steward".into(),
            json!({"adapter": "narrator", "argv": ["narrate", "--json"]}),
        );
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        validate_manifest(&manifest).unwrap();
        assert_eq!(manifest.merge_method, "merge");
        assert_eq!(manifest.steward.as_ref().unwrap().adapter, "narrator");

        // The adapter entry's environment and declared capture ride along; a
        // steward that carried only argv could never be pointed at a real
        // endpoint.
        let mut value = manifest_value_for_test(json!([
            { "id": "task-1", "kind": "implementation", "issue": 8 }
        ]));
        value.as_object_mut().unwrap().insert(
            "steward".into(),
            json!({
                "adapter": "narrator",
                "argv": ["narrate"],
                "env": {"NARRATOR_ENDPOINT": "https://narrator.invalid/v1"},
                "finalMessagePattern": "^NARRATOR_RESULT=(.*)$"
            }),
        );
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        validate_manifest(&manifest).unwrap();
        let steward = manifest.steward.as_ref().unwrap();
        assert_eq!(
            steward.env.get("NARRATOR_ENDPOINT").map(String::as_str),
            Some("https://narrator.invalid/v1")
        );
        assert_eq!(steward.final_message_pattern, "^NARRATOR_RESULT=(.*)$");

        // TALLY_BRIEF is the publish node's own; a steward may not redefine it.
        let mut value = manifest_value_for_test(json!([
            { "id": "task-1", "kind": "implementation", "issue": 8 }
        ]));
        value.as_object_mut().unwrap().insert(
            "steward".into(),
            json!({
                "adapter": "narrator",
                "argv": ["narrate"],
                "env": {"TALLY_BRIEF": "/tmp/x"}
            }),
        );
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("not an assignable environment identifier"),
            "{error}"
        );

        // An empty narration argv would render a steward that cannot be run.
        let mut value = manifest_value_for_test(json!([
            { "id": "task-1", "kind": "implementation", "issue": 8 }
        ]));
        value
            .as_object_mut()
            .unwrap()
            .insert("steward".into(), json!({"adapter": "narrator", "argv": []}));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("steward argv must be a non-empty direct argv"),
            "{error}"
        );
    }

    #[test]
    fn manifest_git_ai_binding_is_off_by_default_and_refuses_unknown_postures() {
        // The shipped state binds nothing. A forge-native manifest that names
        // no posture gets the same integration it always had.
        let tasks = json!([{ "id": "task-1", "kind": "implementation", "issue": 8 }]);
        let manifest: CampaignManifest =
            serde_json::from_value(manifest_value_for_test(tasks.clone())).unwrap();
        assert_eq!(manifest.git_ai_binding, "off");
        assert!(manifest.agent.model.is_none());
        validate_manifest(&manifest).unwrap();

        for posture in ["advisory", "required"] {
            let mut value = manifest_value_for_test(tasks.clone());
            value
                .as_object_mut()
                .unwrap()
                .insert("gitAiBinding".into(), json!(posture));
            let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
            validate_manifest(&manifest).unwrap();
            assert_eq!(manifest.git_ai_binding, posture);
        }

        let mut value = manifest_value_for_test(tasks.clone());
        value
            .as_object_mut()
            .unwrap()
            .insert("gitAiBinding".into(), json!("on"));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("gitAiBinding must be off, advisory, or required"),
            "{error}"
        );

        // The settlement barrier runs inside the merge node, so its budget and
        // that node's deadline are not independent numbers. A pairing that
        // would kill the node mid-await on every task is refused at arm time
        // rather than presenting later as a timeout.
        let mut value = manifest_value_for_test(tasks.clone());
        let object = value.as_object_mut().unwrap();
        object.insert("gitAiBinding".into(), json!("advisory"));
        object.insert("gitAiAwaitSec".into(), json!(60));
        object.insert("driverRuntimeMaxSec".into(), json!(90));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("driverRuntimeMaxSec must be at least twice gitAiAwaitSec (120)"),
            "{error}"
        );

        let mut value = manifest_value_for_test(tasks.clone());
        let object = value.as_object_mut().unwrap();
        object.insert("gitAiBinding".into(), json!("advisory"));
        object.insert("gitAiAwaitSec".into(), json!(12));
        object.insert("driverRuntimeMaxSec".into(), json!(30));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        validate_manifest(&manifest).unwrap();
        assert_eq!(manifest.git_ai_await_sec, 12);

        // With the binding off the two numbers are unrelated again, which is
        // what keeps the shipped default from constraining anyone.
        let mut value = manifest_value_for_test(tasks.clone());
        value
            .as_object_mut()
            .unwrap()
            .insert("driverRuntimeMaxSec".into(), json!(5));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        validate_manifest(&manifest).unwrap();
        assert_eq!(manifest.git_ai_await_sec, 60);

        // An empty model would render a job asking the adapter for nothing at
        // all, and a trailer naming nothing at all.
        let mut value = manifest_value_for_test(tasks);
        value
            .as_object_mut()
            .unwrap()
            .insert("agent".into(), json!({"adapter": "codex", "model": ""}));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        let error = validate_manifest(&manifest).unwrap_err().to_string();
        assert!(
            error.contains("agent limits, policy names, and model must be non-empty and bounded"),
            "{error}"
        );
    }

    #[test]
    fn manifest_accepts_native_checkpoints_and_rejects_unknown_kinds() {
        let value = manifest_value_for_test(json!([
            {
                "id": "build",
                "kind": "implementation",
                "issue": 43,
                "dependencies": [],
                "conflictDomains": []
            },
            {
                "id": "verify",
                "kind": "checkpoint",
                "issue": 44,
                "dependencies": ["build"],
                "argv": ["nix", "flake", "check"],
                "runtimeMaxSec": 900
            }
        ]));
        let manifest: CampaignManifest = serde_json::from_value(value).unwrap();
        validate_manifest(&manifest).unwrap();
        let checkpoint_with_domains = manifest_value_for_test(json!([
            {
                "id": "build",
                "kind": "implementation",
                "issue": 43,
                "dependencies": []
            },
            {
                "id": "verify",
                "kind": "checkpoint",
                "issue": 44,
                "dependencies": ["build"],
                "conflictDomains": [],
                "argv": ["true"],
                "runtimeMaxSec": 30
            }
        ]));
        let manifest: CampaignManifest = serde_json::from_value(checkpoint_with_domains).unwrap();
        assert!(validate_manifest(&manifest).is_err());
        let mut invalid = manifest_value_for_test(json!([{
            "id": "mystery",
            "kind": "approval",
            "issue": 43,
            "dependencies": [],
            "conflictDomains": []
        }]));
        let manifest: CampaignManifest = serde_json::from_value(invalid.take()).unwrap();
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn arm_argv_validation_matches_the_driver() {
        assert!(validate_argv(&["true".into(), "".into()], "argv").is_err());
        assert!(validate_argv(&["true".into(), "line\nbreak".into()], "argv").is_err());
        validate_argv(&["true".into(), "--flag".into()], "argv").unwrap();
    }

    #[test]
    fn canonical_digest_matches_the_driver_contract() {
        let value = json!({"z": [1, "é"], "a": {"b": true, "a": null}});
        assert_eq!(
            sha256_json(&value).unwrap(),
            "sha256:356741b14061aca3cb3e9abc01fe332af042dfcd59d81c56ee9fb57832dc6429"
        );
    }

    /// Rust admits, normalizes, and hashes the graph once. The packaged Python
    /// driver must consume those exact bytes even when the operator spelled a
    /// checkout through a symlink or `..`, and a minimal explicit steward must
    /// already contain every default before it crosses the boundary.
    #[test]
    fn graph_digest_is_byte_identical_between_the_cli_and_the_packaged_driver() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let driver = repo_root.join("drivers/spec_build_driver.py");
        assert!(
            driver.is_file(),
            "packaged driver missing: {}",
            driver.display()
        );

        let temporary = tempfile::tempdir().unwrap();
        let checkout = temporary.path().join("actual-checkout");
        fs::create_dir(&checkout).unwrap();
        let run_git = |directory: &Path, arguments: &[&str]| {
            let output = std::process::Command::new("git")
                .args(arguments)
                .current_dir(directory)
                .output()
                .expect("git must run for campaign parity");
            assert!(
                output.status.success(),
                "git {arguments:?} failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run_git(&checkout, &["init", "--quiet", "--initial-branch=main"]);
        run_git(&checkout, &["config", "user.name", "Tally Test"]);
        run_git(&checkout, &["config", "user.email", "tally-test@invalid"]);
        fs::write(checkout.join("README.md"), "fixture\n").unwrap();
        run_git(&checkout, &["add", "README.md"]);
        run_git(&checkout, &["commit", "--quiet", "-m", "fixture"]);
        let remote = temporary.path().join("remote.git");
        fs::create_dir(&remote).unwrap();
        run_git(
            &remote,
            &["init", "--bare", "--quiet", "--initial-branch=main"],
        );
        run_git(
            &checkout,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        run_git(&checkout, &["push", "--quiet", "-u", "origin", "main"]);

        let symlink = temporary.path().join("checkout-link");
        std::os::unix::fs::symlink(&checkout, &symlink).unwrap();
        let dot_parent = temporary.path().join("spelling-parent");
        fs::create_dir(&dot_parent).unwrap();
        let dotdot = dot_parent.join("..").join("actual-checkout");
        let canonical_checkout = fs::canonicalize(&checkout).unwrap();

        let tasks = vec![CanonicalCampaignTaskV1 {
            number: 101,
            title: "Implement the thing".to_owned(),
            body: "Brief for task-a.".to_owned(),
        }];
        for spelling in [&symlink, &dotdot] {
            let raw_manifest = json!({
                "schemaVersion": 1,
                "name": "parity",
                "repository": {"checkout": spelling, "forge": "local"},
                "pool": "campaign",
                "agent": {},
                "steward": {"adapter": "narrator", "argv": ["narrate"]},
                "gates": [{"kind": "forbidPaths", "id": "gate-forbid", "forbidPaths": ["*.db"]}],
                "tasks": [{"id": "task-a", "kind": "implementation", "issue": 101}]
            });
            let body = format!(
                "{CAMPAIGN_BEGIN}\n```json\n{}\n```\n{CAMPAIGN_END}",
                serde_json::to_string(&raw_manifest).unwrap()
            );
            let manifest =
                parse_manifest(&body, "acme/widgets").expect("arm admission must succeed");
            assert_eq!(manifest.repository.checkout, canonical_checkout);
            let steward = manifest.steward.as_ref().unwrap();
            assert!(steward.env.is_empty());
            assert_eq!(steward.final_message_pattern, "^TALLY_FINAL_MESSAGE=(.*)$");
            assert_eq!(steward.runtime_max_sec, Some(120));
            let graph = CanonicalCampaignGraphV1::new(manifest, tasks.clone()).unwrap();
            let rust_bytes = graph.canonical_json().unwrap();

            let input_path = temporary.path().join(format!(
                "parity-{}.json",
                if spelling == &symlink {
                    "symlink"
                } else {
                    "dotdot"
                }
            ));
            fs::write(
                &input_path,
                serde_json::to_string(&json!({
                    "graph": &graph,
                    "master": {
                        "number": 100,
                        "state": "open",
                        "html_url": "https://github.com/acme/spec/issues/100",
                        "body": "mutable projection body"
                    },
                    "issues": [{
                        "number": 101,
                        "title": "mutable forge title",
                        "body": "mutable forge body",
                        "state": "open",
                        "html_url": "https://github.com/acme/spec/issues/101"
                    }],
                    "admittedIssues": [{
                        "number": 101,
                        "title": "Implement the thing",
                        "body": "Brief for task-a.",
                        "state": "open",
                        "html_url": "https://github.com/acme/spec/issues/101"
                    }]
                }))
                .unwrap(),
            )
            .unwrap();
            let script = r#"
import importlib.util
import json
import sys

spec = importlib.util.spec_from_file_location("spec_build_driver", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with open(sys.argv[2], encoding="utf-8") as handle:
    data = json.load(handle)
decoded = module.canonical_campaign_graph(data["graph"])
responses = iter([data["master"], data["issues"]])
module.github_json = lambda *args, **kwargs: next(responses)
worklist = module.issue_graph_worklist({
    "repository": "acme/spec",
    "issue": {"number": "100", "url": "https://github.com/acme/spec/issues/100"},
    "worklist": {"kind": "github-issue", "graphDigest": decoded["executableDigest"]},
    "armedManifest": decoded["manifest"],
    "campaignGraph": decoded,
})
responses = iter([data["master"], data["admittedIssues"]])
module.github_json = lambda *args, **kwargs: next(responses)
recovered = module.issue_graph_worklist({
    "repository": "acme/spec",
    "issue": {"number": "100", "url": "https://github.com/acme/spec/issues/100"},
    "worklist": {"kind": "github-issue", "graphDigest": decoded["executableDigest"]},
    "armedManifest": decoded["manifest"],
})
print(json.dumps({
    "canonical": module.canonical_json(decoded),
    "digest": decoded["executableDigest"],
    "checkout": str(worklist["config"]["repositoryConfig"]["checkout"]),
    "taskBody": worklist["tasks"][0]["brief"]["body"],
    "taskRevision": worklist["tasks"][0]["revision"],
    "recoveredCheckout": str(recovered["config"]["repositoryConfig"]["checkout"]),
    "recoveredTaskBody": recovered["tasks"][0]["brief"]["body"],
    "recoveredTaskRevision": recovered["tasks"][0]["revision"],
}, sort_keys=True, separators=(",", ":")))
"#;
            let output = std::process::Command::new("python3")
                .args(["-c", script])
                .arg(&driver)
                .arg(&input_path)
                .output()
                .expect("python3 must run the packaged driver for the schema-parity test");
            assert!(
                output.status.success(),
                "packaged driver parity probe failed (status {:?}):\nstdout: {}\nstderr: {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            let observed: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(observed["canonical"], rust_bytes);
            assert_eq!(observed["digest"], graph.executable_digest);
            let expected_task_revision = task_completion_revision(
                &graph.manifest,
                &graph.manifest.tasks[0],
                &graph.tasks[0],
            )
            .unwrap();
            assert_eq!(
                observed["checkout"],
                canonical_checkout.to_str().expect("checkout must be UTF-8")
            );
            assert_eq!(observed["taskBody"], "Brief for task-a.");
            assert_eq!(observed["taskRevision"], expected_task_revision);
            assert_eq!(
                observed["recoveredCheckout"],
                canonical_checkout.to_str().expect("checkout must be UTF-8")
            );
            assert_eq!(observed["recoveredTaskBody"], "Brief for task-a.");
            assert_eq!(observed["recoveredTaskRevision"], expected_task_revision);
        }
    }

    #[tokio::test]
    async fn invalid_manifest_leaves_arm_without_registration_or_enqueue() {
        let temporary = tempfile::tempdir().unwrap();
        let state_dir = temporary.path().join("state");
        let calls = temporary.path().join("gh-calls");
        let master = temporary.path().join("master.json");
        let invalid_manifest = json!({
            "schemaVersion": 1,
            "name": "invalid-arm",
            "repository": {"checkout": "/does/not/matter", "forge": "local"},
            "agent": {},
            "gates": [{
                "kind": "command",
                "id": "test",
                "preflightArgv": ["true"],
                "argv": ["true"],
                "typo": true
            }],
            "tasks": [{"id": "task-a", "kind": "implementation", "issue": 101}]
        });
        let body = format!(
            "{CAMPAIGN_BEGIN}\n```json\n{}\n```\n{CAMPAIGN_END}",
            serde_json::to_string(&invalid_manifest).unwrap()
        );
        fs::write(
            &master,
            serde_json::to_vec(&json!({
                "number": 42,
                "title": "Invalid campaign",
                "body": body,
                "state": "open",
                "html_url": "https://github.com/acme/widgets/issues/42",
                "updated_at": "2026-08-08T00:00:00Z",
                "user": {"login": "operator"}
            }))
            .unwrap(),
        )
        .unwrap();
        let gh = fake_gh(
            temporary.path(),
            "gh-invalid-arm",
            &format!(
                r#"printf '%s\n' "$*" >> '{}'
case "$*" in
  "api user") printf '%s\n' '{{"login":"operator"}}' ;;
  "api repos/acme/widgets/issues/42") cat '{}' ;;
  *) echo "unexpected gh call: $*" >&2; exit 97 ;;
esac"#,
                calls.display(),
                master.display(),
            ),
        );
        let gh_program = GhProgramGuard::acquire();
        gh_program.use_program(&gh);
        write_campaign_projection(
            &state_dir,
            &CampaignProjectionV1 {
                schema_version: CAMPAIGN_PROJECTION_SCHEMA_VERSION,
                code_repository: "acme/widgets".to_owned(),
                worklist_pattern: "specs/night/tasks.json".to_owned(),
                source_revision: "a".repeat(40),
                worklist_sha256: format!("sha256:{}", "b".repeat(64)),
                issue: Some(ProjectedIssueV1 {
                    repository: "acme/widgets".to_owned(),
                    number: 42,
                    url: "https://github.com/acme/widgets/issues/42".to_owned(),
                }),
                sub_issue_walk: None,
            },
        )
        .unwrap();

        let error = run_campaign_arm(
            &temporary.path().join("missing-tally.sock"),
            None,
            Duration::from_secs(1),
            CampaignArmArgs {
                code_repository: "acme/widgets".to_owned(),
                worklist_pattern: "specs/night/tasks.json".to_owned(),
                no_enqueue: false,
                wait: false,
                allowed_actors: Vec::new(),
                allow_test_local_forge: true,
                flow: Some(temporary.path().join("missing-flow.js")),
                driver: Some(temporary.path().join("missing-driver")),
                state_dir: Some(state_dir.clone()),
                workspace_root: None,
                projection_wait_ms: None,
            },
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("unknown field `typo`"),
            "{error}"
        );
        let registration_count = CampaignRegistry::open(&state_dir)
            .unwrap()
            .registrations()
            .unwrap()
            .len();
        assert_eq!(registration_count, 0);
        assert_eq!(
            fs::read_to_string(calls).unwrap().lines().collect::<Vec<_>>(),
            ["api user", "api repos/acme/widgets/issues/42"],
            "invalid admission must stop before sub-issue reads, registration, asset resolution, or queue RPC"
        );
    }

    /// #433: the reconcile digest-mismatch receipt must stop starving the
    /// operator. Two manifests differing in exactly one nested key must yield a
    /// receipt that prints BOTH digests and names that exact canonical path —
    /// presence/shape only, never the value. Removing the path computation from
    /// the driver makes this red, which is the mutation this test pins.
    #[test]
    fn digest_mismatch_receipt_names_both_digests_and_the_first_divergent_path() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let driver = repo_root.join("drivers/spec_build_driver.py");
        assert!(
            driver.is_file(),
            "packaged driver missing: {}",
            driver.display()
        );

        // One agent, shared shape; the live side carries exactly one extra
        // nested key, exactly the #429 skew shape.
        let agent = json!({
            "adapter": "codex",
            "argv": [BRIEF_SENTINEL],
            "priority": "low",
            "runtimeMaxSec": 14_400,
            "approvalPolicy": "never",
            "sandboxPolicy": "danger-full-access",
            "model": null
        });
        let armed_agent = agent.clone();
        let mut live_agent = agent.clone();
        live_agent["diagnosisSandboxPolicy"] = json!("read-only");
        let manifest = |agent: Value| {
            json!({
                "schemaVersion": 1,
                "name": "parity",
                "agent": agent,
                "tasks": []
            })
        };
        let armed = manifest(armed_agent);
        let live = manifest(live_agent);
        let tasks = json!([]);

        let input_dir = tempfile::tempdir().unwrap();
        let input_path = input_dir.path().join("divergence.json");
        fs::write(
            &input_path,
            serde_json::to_string(&json!({"armed": armed, "live": live, "tasks": tasks})).unwrap(),
        )
        .unwrap();
        let script = r#"
import importlib.util
import json
import sys

spec = importlib.util.spec_from_file_location("spec_build_driver", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
with open(sys.argv[2], encoding="utf-8") as handle:
    data = json.load(handle)
tasks = data["tasks"]
armed_digest = module.canonical_sha256({"manifest": data["armed"], "tasks": tasks})
live_digest = module.canonical_sha256({"manifest": data["live"], "tasks": tasks})
receipt = module.graph_digest_mismatch_receipt(
    data["armed"], data["live"], armed_digest, live_digest
)
path = module.first_divergent_canonical_path(data["armed"], data["live"])
print(json.dumps({
    "armedDigest": armed_digest,
    "liveDigest": live_digest,
    "receipt": receipt,
    "path": path,
}))
"#;
        let output = std::process::Command::new("python3")
            .args(["-c", script])
            .arg(&driver)
            .arg(&input_path)
            .output()
            .expect("python3 must run the packaged driver for the divergence test");
        assert!(
            output.status.success(),
            "packaged driver divergence probe failed (status {:?}):\nstdout: {}\nstderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let probe: Value = serde_json::from_str(
            &String::from_utf8(output.stdout).expect("divergence probe must print UTF-8"),
        )
        .expect("divergence probe must print JSON");

        let armed_digest = probe["armedDigest"].as_str().unwrap();
        let live_digest = probe["liveDigest"].as_str().unwrap();
        let receipt = probe["receipt"].as_str().unwrap();
        assert_ne!(armed_digest, live_digest);
        assert_eq!(
            probe["path"].as_str().unwrap(),
            "agent.diagnosisSandboxPolicy: absent-in-armed / present-in-live"
        );
        // Both digests, in the arm CLI's `sha256:` form.
        assert!(receipt.contains(armed_digest), "{receipt}");
        assert!(receipt.contains(live_digest), "{receipt}");
        // The first divergent canonical path, prefixed under the manifest.
        assert!(
            receipt.contains(
                "manifest.agent.diagnosisSandboxPolicy: absent-in-armed / present-in-live"
            ),
            "{receipt}"
        );
        // The existing instruction survives: this adds evidence, it does not
        // change the verdict.
        assert!(
            receipt.contains("inspect it and explicitly re-arm"),
            "{receipt}"
        );
        // The receipt must not widen what it publishes: the withheld value
        // never appears.
        assert!(!receipt.contains("read-only"), "{receipt}");
    }

    #[test]
    fn registration_v3_round_trips_local_authority() {
        let root = tempfile::tempdir().unwrap();
        let state_dir = root.path();
        let forge_actor = "operator".to_owned();
        let flow = root.path().join("flow.js");
        let driver = root.path().join("driver");
        fs::write(&flow, "flow fixture\n").unwrap();
        fs::write(&driver, "driver fixture\n").unwrap();
        let mut registration = CampaignRegistration::new(
            CampaignRegistrationV3 {
                schema_version: REGISTRY_SCHEMA_VERSION,
                registration_id: uuid::Uuid::now_v7().to_string(),
                worklist_pattern: "specs/night/tasks.json".to_owned(),
                code_repository: "acme/widgets".to_owned(),
                armed_at: "2026-08-01T00:00:00Z".to_owned(),
                arm_serial: 1,
                approved_graph_digest: format!("sha256:{}", "a".repeat(64)),
                local_actor: local_actor(),
                allowed_actors: normalize_allowed_actors(&["Reviewer".into()], &forge_actor)
                    .unwrap(),
                last_observation: None,
                flow,
                driver,
                workspace_root: PathBuf::from("/srv/tally-campaigns"),
            },
            Some(240_000),
        );
        let registry = CampaignRegistry::open(state_dir).unwrap();
        registry.write(&mut registration).unwrap();
        let loaded = registry
            .read_campaign(
                &registration.code_repository,
                &registration.worklist_pattern,
            )
            .unwrap()
            .unwrap();
        assert_eq!(loaded.registration_id, registration.registration_id);
        assert_eq!(loaded.allowed_actors, ["operator", "reviewer"]);
        assert_eq!(
            loaded.approved_graph_digest,
            registration.approved_graph_digest
        );
        // #432: the durable projection wait survives the round trip, which is
        // what makes `campaign poll` dispatch later passes with the same
        // widened window the operator armed with.
        assert_eq!(loaded.projection_wait_ms, Some(240_000));
    }

    #[test]
    fn approved_graph_snapshots_are_generation_scoped_and_digest_checked() {
        let temporary = tempfile::tempdir().unwrap();
        let prior = canonical_graph_for_pardon(&[]);
        let amended = canonical_graph_for_pardon(&["prerequisite"]);
        let mut registration = CampaignRegistration::new(
            CampaignRegistrationV3 {
                schema_version: REGISTRY_SCHEMA_VERSION,
                registration_id: uuid::Uuid::now_v7().to_string(),
                worklist_pattern: "specs/night/tasks.json".to_owned(),
                code_repository: "acme/widgets".to_owned(),
                armed_at: "2026-08-01T00:00:00Z".to_owned(),
                arm_serial: 1,
                approved_graph_digest: prior.executable_digest.clone(),
                local_actor: local_actor(),
                allowed_actors: vec!["operator".to_owned()],
                last_observation: None,
                flow: PathBuf::from("/nix/store/flow.js"),
                driver: PathBuf::from("/nix/store/driver"),
                workspace_root: PathBuf::from("/srv/tally-campaigns"),
            },
            None,
        );

        assert!(
            read_approved_graph_snapshot(temporary.path(), &registration)
                .unwrap()
                .is_none(),
            "a pre-snapshot registration remains readable and simply cannot prove an amendment"
        );
        write_approved_graph_snapshot(temporary.path(), &registration, &prior).unwrap();
        assert_eq!(
            read_approved_graph_snapshot(temporary.path(), &registration)
                .unwrap()
                .unwrap(),
            prior
        );
        let old_path = approved_graph_path(temporary.path(), &registration);

        registration.arm_serial = 2;
        registration.approved_graph_digest = amended.executable_digest.clone();
        write_approved_graph_snapshot(temporary.path(), &registration, &amended).unwrap();
        assert_eq!(
            read_approved_graph_snapshot(temporary.path(), &registration)
                .unwrap()
                .unwrap(),
            amended
        );
        prune_approved_graph_snapshots(temporary.path(), &registration).unwrap();
        assert!(
            !old_path.exists(),
            "the superseded graph generation must be pruned"
        );
    }

    /// #432 acceptance 2, the DELIVERY half of the seam.
    ///
    /// Recording `--projection-wait-ms` in the registration is worth nothing on
    /// its own: what the operator is promised is that every pass this campaign
    /// dispatches waits that long. `CampaignHost::dispatch_flow_argv` is the
    /// only place that promise is kept, so it is asserted here directly — a
    /// registration carrying `Some(n)` must put
    /// `--result-projection-wait-ms n` on the dispatched pass's argv, spelled
    /// exactly as `FlowRunArgs` parses it.
    ///
    /// The `None` half is not decoration. This argv is hashed into the enqueue
    /// payload, so a stray element would move the payload identity of every
    /// campaign armed without the flag; it is asserted element-by-element.
    ///
    /// Deleting the `--result-projection-wait-ms` push from the host's dispatch
    /// argv makes this test red — that mutation used to leave the whole crate
    /// green.
    #[test]
    fn a_recorded_projection_wait_reaches_the_dispatched_pass_argv() {
        let executable = Path::new("/nix/store/tally/bin/tally");
        let flow = Path::new("/nix/store/spec-build.js");
        let host = CampaignHost {
            socket: Path::new("/run/user/1000/tally/tally.sock"),
            config_path: None,
            state_dir: Path::new("/home/operator/.local/state/tally"),
            rpc_timeout: Duration::from_secs(30),
        };

        // No projection-wait flag means no projection-wait elements. A host
        // without an explicit config likewise emits no --config pair, while
        // the socket locator still precedes the flow subcommand.
        let unset = host.dispatch_flow_argv(executable, flow, 51, None);
        assert_eq!(
            unset,
            vec![
                "/nix/store/tally/bin/tally".to_owned(),
                "--socket".to_owned(),
                "/run/user/1000/tally/tally.sock".to_owned(),
                "flow".to_owned(),
                "run".to_owned(),
                "/nix/store/spec-build.js".to_owned(),
                "--args-from-brief".to_owned(),
                "--max-nodes".to_owned(),
                "51".to_owned(),
            ],
            "a campaign armed without --projection-wait-ms must dispatch the \
             same argv without projection-wait elements; this vector is hashed \
             into the enqueue payload"
        );

        // The recorded wait, delivered.
        let widened = host.dispatch_flow_argv(executable, flow, 51, Some(240_000));
        assert_eq!(
            widened,
            [
                unset.as_slice(),
                &[
                    "--result-projection-wait-ms".to_owned(),
                    "240000".to_owned()
                ]
            ]
            .concat(),
            "a registration carrying a projection wait must put it on the \
             dispatched pass's argv"
        );

        // The flag this argv names must be the flag `flow run` parses, or the
        // dispatched pass dies on an unknown argument instead of waiting.
        let parsed = Opts::try_parse_from(widened.iter().map(String::as_str))
            .expect("the dispatched argv must parse as a tally invocation");
        assert!(matches!(
            parsed.command,
            Some(Command::Flow {
                command: FlowCommand::Run(FlowRunArgs {
                    args_from_brief: true,
                    max_nodes: 51,
                    result_projection_wait_ms: Some(240_000),
                    ..
                })
            })
        ));
    }

    /// #432, the arm-side half of the refusal (the flow-side zero and
    /// unparsable refusals are pinned in `cli::flow::tests`). A zero recorded
    /// here would be durable: every pass this campaign ever dispatches,
    /// including the unattended poll ones, would then die on its own argv.
    #[test]
    fn a_zero_projection_wait_is_refused_at_arm() {
        assert_eq!(validated_projection_wait_ms(None).unwrap(), None);
        assert_eq!(
            validated_projection_wait_ms(Some(240_000)).unwrap(),
            Some(240_000)
        );
        assert_eq!(validated_projection_wait_ms(Some(1)).unwrap(), Some(1));
        let refused = validated_projection_wait_ms(Some(0)).unwrap_err();
        assert!(
            refused.to_string().contains("--projection-wait-ms"),
            "{refused}"
        );
    }

    /// #432 acceptance 2, the seam that actually reaches a campaign pass. A
    /// registration written before `--projection-wait-ms` existed carries no
    /// field at all; it must still load with the historical 10-second value
    /// rather than being refused or defaulted to zero.
    #[test]
    fn a_registration_without_a_projection_wait_still_loads() {
        let root = tempfile::tempdir().unwrap();
        let state_dir = root.path();
        let code_repository = "acme/widgets";
        let worklist_pattern = "specs/night/tasks.json";
        let registry = CampaignRegistry::open(state_dir).unwrap();
        let path = registry.registration_path(code_repository, worklist_pattern);
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "schemaVersion": REGISTRY_SCHEMA_VERSION,
                "registrationId": uuid::Uuid::now_v7().to_string(),
                "worklistPattern": worklist_pattern,
                "codeRepository": code_repository,
                "armedAt": "2026-08-01T00:00:00Z",
                "armSerial": 1,
                "approvedGraphDigest": format!("sha256:{}", "a".repeat(64)),
                "localActor": local_actor(),
                "allowedActors": ["operator"],
                "flow": "/nix/store/flow.js",
                "driver": "/nix/store/driver",
                "workspaceRoot": "/srv/tally-campaigns",
            }))
            .unwrap(),
        )
        .unwrap();
        let loaded = registry.read(&path).unwrap();
        assert_eq!(
            loaded.projection_wait_ms,
            Some(tally_core::campaign_registry::DEFAULT_CAMPAIGN_PROJECTION_WAIT_MS)
        );
    }

    fn codex_shaped_adapter(commit_capable: &[&str]) -> AdapterConfig {
        AdapterConfig {
            argv: vec![
                "codex".to_owned(),
                "exec".to_owned(),
                "--json".to_owned(),
                "--".to_owned(),
            ],
            launch: tally_core::adapters::AdapterLaunchConfig {
                approval_policies: BTreeMap::from([(
                    "never".to_owned(),
                    vec!["-c".to_owned(), "approval_policy=\"never\"".to_owned()],
                )]),
                sandbox_policies: BTreeMap::from([
                    (
                        "workspace-write".to_owned(),
                        vec!["--sandbox".to_owned(), "workspace-write".to_owned()],
                    ),
                    (
                        "danger-full-access".to_owned(),
                        vec!["--sandbox".to_owned(), "danger-full-access".to_owned()],
                    ),
                ]),
                commit_capable_sandbox_policies: commit_capable
                    .iter()
                    .map(|policy| (*policy).to_owned())
                    .collect(),
                ..tally_core::adapters::AdapterLaunchConfig::default()
            },
            ..AdapterConfig::default()
        }
    }

    fn agent_with(sandbox: Option<&str>) -> CampaignAgent {
        let mut agent: CampaignAgent = serde_json::from_value(json!({})).unwrap();
        agent.sandbox_policy = sandbox.map(str::to_owned);
        agent
    }

    #[test]
    fn campaign_defaults_are_a_pairing_a_codex_agent_can_commit_under() {
        let adapter = codex_shaped_adapter(&["danger-full-access"]);
        // The shipped module defaults, unmodified.
        let defaults = agent_with(Some(DEFAULT_AGENT_SANDBOX_POLICY));
        assert_eq!(defaults.approval_policy.as_deref(), Some("never"));
        validate_agent_policies(&defaults, &adapter).unwrap();

        // The estate workaround already deployed by the consumer: both values
        // explicit, approval disabled outright.
        let workaround = CampaignAgent {
            approval_policy: None,
            ..agent_with(Some("danger-full-access"))
        };
        validate_agent_policies(&workaround, &adapter).unwrap();
    }

    #[test]
    fn missing_agent_final_message_capture_warns_before_worker_findings_are_lost() {
        let agent = agent_with(Some(DEFAULT_AGENT_SANDBOX_POLICY));
        let mut adapter = codex_shaped_adapter(&["danger-full-access"]);
        let warning = worker_findings_warning(&agent, &adapter).unwrap();
        assert!(warning.contains("scrape.finalMessage"), "{warning}");
        assert!(
            warning.contains("worker findings will not be retained"),
            "{warning}"
        );

        adapter.scrape.insert(
            "finalMessage".to_owned(),
            serde_json::from_value(json!({
                "mode": "jsonPathLast",
                "pattern": "$[?@.type == 'item.completed'].item.text"
            }))
            .unwrap(),
        );
        assert_eq!(worker_findings_warning(&agent, &adapter), None);
    }

    fn manifest_with_checkpoint_and_gate_argv(argv: Vec<String>) -> CampaignManifest {
        let mut value = manifest_value_for_test(json!([{
            "id": "checkpoint",
            "kind": "checkpoint",
            "issue": 43,
            "dependencies": [],
            "argv": argv,
            "runtimeMaxSec": 900
        }]));
        value["gates"] = json!([{
            "kind": "command",
            "id": "flake-check",
            "preflightArgv": value["tasks"][0]["argv"].clone(),
            "argv": value["tasks"][0]["argv"].clone(),
            "runtimeMaxSec": 900
        }]);
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn argv_hazards_are_silent_without_a_hardening_tier() {
        let hazardous = vec![
            "sh".to_owned(),
            "-euc".to_owned(),
            "nix build /tmp/staged; mkdir -p \"$HOME/output\"".to_owned(),
        ];
        assert!(
            argv_hazard_warnings(
                &manifest_with_checkpoint_and_gate_argv(hazardous),
                AdapterHardening::None,
            )
            .is_empty(),
            "a host without a hardening preset must not receive hardened-tier argv warnings"
        );
    }

    #[test]
    fn hardened_argv_hazards_warn_for_checkpoints_and_gates_but_hermetic_argv_is_silent() {
        let bare_nix = vec!["nix".to_owned(), "build".to_owned(), ".#checks".to_owned()];
        let hermetic_nix = vec![
            "sh".to_owned(),
            "-euc".to_owned(),
            "export XDG_CACHE_HOME=/tmp/nix-cache XDG_STATE_HOME=/tmp/nix-state; mkdir -p \"$XDG_CACHE_HOME\" \"$XDG_STATE_HOME\"; exec nix build .#checks".to_owned(),
        ];

        let warnings = argv_hazard_warnings(
            &manifest_with_checkpoint_and_gate_argv(bare_nix),
            AdapterHardening::Strict,
        );
        assert_eq!(warnings.len(), 3, "{warnings:#?}");
        for context in [
            "checkpoint task \"checkpoint\" argv",
            "campaign gate \"flake-check\" preflightArgv",
            "campaign gate \"flake-check\" argv",
        ] {
            assert!(
                warnings
                    .iter()
                    .any(|warning| warning.contains(context) && warning.contains("nix")),
                "missing warning for {context}: {warnings:#?}"
            );
        }

        assert!(
            argv_hazard_warnings(
                &manifest_with_checkpoint_and_gate_argv(hermetic_nix),
                AdapterHardening::Strict,
            )
            .is_empty(),
            "the documented private-cache argv must not warn"
        );
    }

    #[test]
    fn argv_hazards_ignore_self_created_tmp_paths_and_non_evaluating_nix_probes() {
        let benign = vec![
            "sh".to_owned(),
            "-euc".to_owned(),
            "command -v nix >/dev/null; mkdir -p /tmp/tally-gate; test -d /tmp/tally-gate/output"
                .to_owned(),
        ];
        assert!(
            argv_hazard_warnings(
                &manifest_with_checkpoint_and_gate_argv(benign),
                AdapterHardening::Production,
            )
            .is_empty(),
            "in-unit /tmp creation and a nix availability probe are safe under PrivateTmp"
        );

        for subcommand in ["develop", "build", "shell", "run"] {
            let warnings = argv_hazard_warnings(
                &manifest_with_checkpoint_and_gate_argv(vec![
                    "nix".to_owned(),
                    subcommand.to_owned(),
                ]),
                AdapterHardening::Workspace,
            );
            assert_eq!(warnings.len(), 3, "nix {subcommand}: {warnings:#?}");
        }

        for staged in [
            "mkdir -p /tmp/owned; cat /tmp/staged",
            "cat /tmp/late; mkdir -p /tmp/late",
        ] {
            assert!(
                argv_has_staged_tmp_reference(&[
                    "sh".to_owned(),
                    "-c".to_owned(),
                    staged.to_owned()
                ]),
                "an unrelated or later mkdir must not suppress {staged:?}"
            );
        }
    }

    #[test]
    fn a_sandbox_that_cannot_commit_is_refused_at_arm_time() {
        let adapter = codex_shaped_adapter(&["danger-full-access"]);
        for sandbox in [Some("workspace-write"), None] {
            let error = validate_agent_policies(&agent_with(sandbox), &adapter)
                .unwrap_err()
                .to_string();
            assert!(error.contains("cannot create a commit"), "{error}");
            assert!(error.contains("danger-full-access"), "{error}");
        }

        // An adapter that declares no commit capability is not second-guessed.
        let silent = codex_shaped_adapter(&[]);
        validate_agent_policies(&agent_with(Some("workspace-write")), &silent).unwrap();
        validate_agent_policies(&agent_with(None), &silent).unwrap();
    }

    #[test]
    fn an_undeclared_policy_name_is_still_refused_at_arm_time() {
        let adapter = codex_shaped_adapter(&["danger-full-access"]);
        let error = validate_agent_policies(&agent_with(Some("read-only")), &adapter)
            .unwrap_err()
            .to_string();
        assert!(error.contains("is not authorized by adapter"), "{error}");
    }
}
