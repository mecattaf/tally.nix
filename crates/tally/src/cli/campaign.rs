use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::process::{Command as ProcessCommand, Stdio};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tally_core::adapters::AdapterConfig;
use tally_core::config::ResourceKind;

const CAMPAIGN_SCHEMA_VERSION: u32 = 1;
const CAMPAIGN_BEGIN: &str = "<!-- tally:campaign:v1 -->";
const CAMPAIGN_END: &str = "<!-- tally:campaign:v1:end -->";
const WORKLIST_BEGIN: &str = "<!-- tally:campaign-worklist:v1 -->";
const WORKLIST_END: &str = "<!-- tally:campaign-worklist:v1:end -->";
const TASK_MARKER_PREFIX: &str = "<!-- tally:campaign-task:v1 id=";
const DEFAULT_MAX_TASKS: usize = 64;
const DEFAULT_DRIVER_RUNTIME_MAX_SEC: u64 = 900;
const DEFAULT_AGENT_RUNTIME_MAX_SEC: u64 = 14_400;
const DEFAULT_RUNNER_POOL: &str = "campaign";
const DEFAULT_AGENT_PRIORITY: &str = "low";
const DEFAULT_AGENT_APPROVAL_POLICY: &str = "never";
const DEFAULT_AGENT_SANDBOX_POLICY: &str = "danger-full-access";
const DEFAULT_AGENT_DIAGNOSIS_SANDBOX_POLICY: &str = "read-only";
const BRIEF_SENTINEL: &str = "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set.";
const REGISTRY_SCHEMA_VERSION: u32 = 2;
const SYSTEM_COMMENT_PREFIX: &str = "<!-- tally:spec-build:";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignRepository {
    checkout: PathBuf,
    #[serde(default = "default_base_branch")]
    base_branch: String,
    #[serde(default = "default_remote")]
    remote: String,
    #[serde(default = "default_forge")]
    forge: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignAgent {
    #[serde(default = "default_agent_adapter")]
    adapter: String,
    #[serde(default = "default_agent_argv")]
    argv: Vec<String>,
    #[serde(default = "default_agent_priority")]
    priority: String,
    #[serde(default = "default_agent_runtime_max_sec")]
    runtime_max_sec: Option<u64>,
    #[serde(default = "default_agent_approval_policy")]
    approval_policy: Option<String>,
    #[serde(default = "default_agent_sandbox_policy")]
    sandbox_policy: Option<String>,
    /// Named adapter sandbox policy for diagnosis nodes. The diagnosis brief
    /// prohibits mutation, so the default holds that node to a read-only
    /// policy rather than inheriting the implementation node's writable one.
    /// The packaged driver normalizes this exact field and default into the
    /// canonical agent it digests, so both halves of the pin must carry it
    /// byte-identically (#429).
    #[serde(default = "default_agent_diagnosis_sandbox_policy")]
    diagnosis_sandbox_policy: Option<String>,
    /// The model this campaign dispatches its coder with. Absent leaves the
    /// adapter's own resolution alone and leaves the merge node with no model
    /// to name in an `Assisted-by:` trailer.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum CampaignGate {
    #[serde(rename = "command", rename_all = "camelCase")]
    Command {
        id: String,
        preflight_argv: Vec<String>,
        argv: Vec<String>,
        #[serde(default = "default_gate_runtime_max_sec")]
        runtime_max_sec: u64,
    },
    #[serde(rename = "forbidPaths", rename_all = "camelCase")]
    ForbidPaths {
        id: String,
        forbid_paths: Vec<String>,
        #[serde(default = "default_gate_runtime_max_sec")]
        runtime_max_sec: u64,
    },
}

impl CampaignGate {
    fn id(&self) -> &str {
        match self {
            Self::Command { id, .. } | Self::ForbidPaths { id, .. } => id,
        }
    }

    const fn is_command(&self) -> bool {
        matches!(self, Self::Command { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignTaskReference {
    id: String,
    kind: String,
    issue: u64,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    conflict_domains: Vec<String>,
    #[serde(default)]
    argv: Option<Vec<String>>,
    #[serde(default)]
    runtime_max_sec: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignManifest {
    schema_version: u32,
    name: String,
    repository: CampaignRepository,
    #[serde(default = "default_max_tasks")]
    max_tasks: usize,
    #[serde(default = "default_max_parallel")]
    max_parallel: usize,
    #[serde(default = "default_driver_runtime_max_sec")]
    driver_runtime_max_sec: u64,
    #[serde(default = "default_campaign_runtime_max_sec")]
    runtime_max_sec: Option<u64>,
    #[serde(default = "default_runner_pool")]
    pool: String,
    /// How the merge node integrates a task. Squash is the campaign default:
    /// the exposed footprint is one conventional commit per task, and a merge
    /// commit carrying a template message is not that.
    #[serde(default = "default_merge_method")]
    merge_method: String,
    /// Whether the merge node binds Git AI authorship on the commit it just
    /// integrated. `off` is the shipped state; `advisory` records the outcome
    /// as a merge receipt and never fails the node; `required` fails it.
    #[serde(default = "default_git_ai_binding")]
    git_ai_binding: String,
    /// How long the merge node may wait on git-ai's settlement barrier. The
    /// barrier runs inside that node, so a deadline that does not comfortably
    /// exceed this budget is refused while the binding is armed.
    #[serde(default = "default_git_ai_await_sec")]
    git_ai_await_sec: u64,
    agent: CampaignAgent,
    /// The steward bound as a catalog role. Absent leaves the narrate slot
    /// empty: publication text stays the brief-derived template.
    #[serde(default)]
    steward: Option<CampaignSteward>,
    gates: Vec<CampaignGate>,
    tasks: Vec<CampaignTaskReference>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignSteward {
    adapter: String,
    argv: Vec<String>,
    /// The adapter entry's environment: where a narrator's endpoint and
    /// credentials live. Empty is the shipped state.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// The adapter's declared final-message capture. Absent means the shipped
    /// `^TALLY_FINAL_MESSAGE=(.*)$` contract.
    #[serde(default)]
    final_message_pattern: Option<String>,
    #[serde(default)]
    runtime_max_sec: Option<u64>,
}

#[derive(Debug, Clone)]
struct ProjectTask {
    id: String,
    kind: String,
    title: String,
    body: String,
    issue: Option<u64>,
    dependencies: Vec<String>,
    conflict_domains: Vec<String>,
    argv: Option<Vec<String>>,
    runtime_max_sec: Option<u64>,
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

#[derive(Debug, Clone)]
struct CampaignGraph {
    locator: IssueLocator,
    manifest: CampaignManifest,
    master: GithubIssue,
    tasks: Vec<GithubIssue>,
    executable_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CampaignRegistration {
    schema_version: u32,
    registration_id: String,
    issue_url: String,
    repository: String,
    issue_number: u64,
    armed_at: String,
    arm_serial: u64,
    approved_graph_digest: String,
    authenticated_actor: String,
    allowed_actors: Vec<String>,
    allow_test_local_forge: bool,
    /// What the arm-time probe found this forge can serve. A registration
    /// written before the probe existed carries no record, and absent means
    /// degraded — the conservative direction, since the checkbox projection
    /// and per-branch pull-request lookup work on every forge. Re-arming is
    /// what turns the native sub-issue walk on.
    #[serde(default)]
    sub_issue_walk: bool,
    #[serde(default)]
    last_observation: Option<String>,
    /// The cheap half of the last observation: everything the poll can see
    /// from the two REST reads it makes anyway. Absent on a registration
    /// written before this existed, which only costs one extra walk.
    #[serde(default)]
    last_forge_observation: Option<String>,
    /// How long each pass of this campaign waits for a node's advisory
    /// finalMessage projection before classifying the node
    /// `retryable-projection` (#432). Absent leaves the flow host's own 10 s
    /// default alone, which is what a registration written before this
    /// existed carries and what every campaign armed without the flag carries.
    /// It lives here rather than in the manifest because it is a property of
    /// this host's daemon congestion, not of the campaign: putting it in the
    /// manifest would fold a host tuning knob into the executable graph digest
    /// and force a re-arm to change it.
    #[serde(default)]
    projection_wait_ms: Option<u64>,
    flow: PathBuf,
    driver: PathBuf,
    workspace_root: PathBuf,
}

fn default_base_branch() -> String {
    "main".to_owned()
}

fn default_remote() -> String {
    "origin".to_owned()
}

fn default_forge() -> String {
    "github".to_owned()
}

fn default_agent_adapter() -> String {
    "codex".to_owned()
}

fn default_agent_argv() -> Vec<String> {
    vec![BRIEF_SENTINEL.to_owned()]
}

fn default_agent_priority() -> String {
    DEFAULT_AGENT_PRIORITY.to_owned()
}

const fn default_agent_runtime_max_sec() -> Option<u64> {
    Some(DEFAULT_AGENT_RUNTIME_MAX_SEC)
}

fn default_agent_approval_policy() -> Option<String> {
    Some(DEFAULT_AGENT_APPROVAL_POLICY.to_owned())
}

fn default_agent_sandbox_policy() -> Option<String> {
    Some(DEFAULT_AGENT_SANDBOX_POLICY.to_owned())
}

fn default_agent_diagnosis_sandbox_policy() -> Option<String> {
    Some(DEFAULT_AGENT_DIAGNOSIS_SANDBOX_POLICY.to_owned())
}

const fn default_gate_runtime_max_sec() -> u64 {
    900
}

const fn default_max_tasks() -> usize {
    DEFAULT_MAX_TASKS
}

const fn default_max_parallel() -> usize {
    1
}

const fn default_driver_runtime_max_sec() -> u64 {
    DEFAULT_DRIVER_RUNTIME_MAX_SEC
}

const fn default_campaign_runtime_max_sec() -> Option<u64> {
    Some(86_400)
}

fn default_merge_method() -> String {
    "squash".to_owned()
}

fn default_git_ai_binding() -> String {
    "off".to_owned()
}

const fn default_git_ai_await_sec() -> u64 {
    60
}

fn default_runner_pool() -> String {
    DEFAULT_RUNNER_POOL.to_owned()
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
        CampaignCommand::Project(args) => run_campaign_project(args),
        CampaignCommand::Poll(args) => {
            run_campaign_poll(socket, config_path, rpc_timeout, args).await
        }
        CampaignCommand::List(args) => run_campaign_list(args),
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

fn require_authenticated_actor(registration: &CampaignRegistration) -> Result<()> {
    let actor = authenticated_actor()?;
    if actor != registration.authenticated_actor {
        bail!(
            "armed campaign {} was approved by gh actor {:?}, but the current gh actor is {:?}; re-arm explicitly with the intended identity",
            registration.issue_url,
            registration.authenticated_actor,
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

fn fetch_steering(locator: &IssueLocator, allowed: &[String]) -> Result<Vec<Value>> {
    let endpoint = format!(
        "repos/{}/issues/{}/comments?per_page=100",
        locator.repository, locator.number
    );
    let pages: Vec<Vec<GithubComment>> =
        gh_json(&os_arguments(&["api", "--paginate", "--slurp", &endpoint]))?;
    let allowed = allowed.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut comments = Vec::new();
    for comment in pages.into_iter().flatten() {
        if comment.body.contains(SYSTEM_COMMENT_PREFIX) {
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
            if comment.body.contains(SYSTEM_COMMENT_PREFIX) {
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

fn parse_manifest(body: &str) -> Result<CampaignManifest> {
    let section = extract_managed_section(body, CAMPAIGN_BEGIN, CAMPAIGN_END)?;
    let json = section
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .ok_or_else(|| invalid("campaign manifest must be one fenced JSON object"))?;
    let manifest: CampaignManifest = serde_json::from_str(json)
        .map_err(|error| invalid(format!("campaign manifest is invalid: {error}")))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &CampaignManifest) -> Result<()> {
    if manifest.schema_version != CAMPAIGN_SCHEMA_VERSION {
        return Err(invalid("campaign manifest schemaVersion must equal 1"));
    }
    if !safe_component(&manifest.name) {
        return Err(invalid("campaign name is not a safe component"));
    }
    if !manifest.repository.checkout.is_absolute() {
        return Err(invalid("campaign repository.checkout must be absolute"));
    }
    if manifest.repository.base_branch.is_empty() || manifest.repository.base_branch.contains('\0')
    {
        return Err(invalid("campaign repository.baseBranch must be non-empty"));
    }
    if !safe_component(&manifest.repository.remote) {
        return Err(invalid(
            "campaign repository.remote is not a safe Git remote name",
        ));
    }
    if !matches!(manifest.repository.forge.as_str(), "github" | "local") {
        return Err(invalid("campaign repository.forge must be github or local"));
    }
    if !(1..=100).contains(&manifest.max_tasks) {
        return Err(invalid("forge-native campaign maxTasks must be in 1..=100"));
    }
    if !(1..=manifest.max_tasks).contains(&manifest.max_parallel) {
        return Err(invalid(
            "campaign maxParallel must be positive and not exceed maxTasks",
        ));
    }
    if manifest.driver_runtime_max_sec == 0 || manifest.runtime_max_sec == Some(0) {
        return Err(invalid(
            "campaign runtime limits must be positive when present",
        ));
    }
    if !safe_component(&manifest.pool) {
        return Err(invalid("campaign pool is not a safe component"));
    }
    if !matches!(manifest.merge_method.as_str(), "merge" | "squash") {
        return Err(invalid("campaign mergeMethod must be merge or squash"));
    }
    if !matches!(
        manifest.git_ai_binding.as_str(),
        "off" | "advisory" | "required"
    ) {
        return Err(invalid(
            "campaign gitAiBinding must be off, advisory, or required",
        ));
    }
    if manifest.git_ai_await_sec == 0 {
        return Err(invalid("campaign gitAiAwaitSec must be positive"));
    }
    if manifest.git_ai_binding != "off"
        && manifest.driver_runtime_max_sec < 2 * manifest.git_ai_await_sec
    {
        return Err(invalid(format!(
            "campaign driverRuntimeMaxSec must be at least twice gitAiAwaitSec ({}) while gitAiBinding is not off",
            2 * manifest.git_ai_await_sec
        )));
    }
    validate_agent(&manifest.agent)?;
    if let Some(steward) = &manifest.steward {
        if !safe_component(&steward.adapter) {
            return Err(invalid("campaign steward adapter is not a safe component"));
        }
        if steward.argv.is_empty() || steward.argv.iter().any(|item| item.is_empty()) {
            return Err(invalid(
                "campaign steward argv must be non-empty and contain no empty values",
            ));
        }
        if steward.runtime_max_sec == Some(0) {
            return Err(invalid("campaign steward runtimeMaxSec must be positive"));
        }
        for name in steward.env.keys() {
            if name.is_empty()
                || name == "TALLY_BRIEF"
                || !name.chars().enumerate().all(|(index, character)| {
                    character == '_'
                        || character.is_ascii_alphabetic()
                        || (index > 0 && character.is_ascii_digit())
                })
            {
                return Err(invalid(format!(
                    "campaign steward env name {name:?} is not an assignable environment identifier"
                )));
            }
        }
        if steward
            .final_message_pattern
            .as_ref()
            .is_some_and(|pattern| pattern.is_empty())
        {
            return Err(invalid(
                "campaign steward finalMessagePattern must be non-empty when set",
            ));
        }
    }
    validate_gates(&manifest.gates)?;
    if manifest.tasks.is_empty() || manifest.tasks.len() > manifest.max_tasks {
        return Err(invalid(format!(
            "campaign must contain 1..={} tasks",
            manifest.max_tasks
        )));
    }
    let mut prior = BTreeSet::new();
    let mut issues = BTreeSet::new();
    for task in &manifest.tasks {
        if !matches!(task.kind.as_str(), "implementation" | "checkpoint") {
            return Err(invalid(format!(
                "campaign task {} kind must be implementation or checkpoint",
                task.id
            )));
        }
        if !safe_task_id(&task.id) {
            return Err(invalid(format!(
                "campaign task id {:?} is invalid",
                task.id
            )));
        }
        if !prior.insert(task.id.clone()) {
            return Err(invalid(format!("campaign repeats task id {:?}", task.id)));
        }
        if task.issue == 0 || !issues.insert(task.issue) {
            return Err(invalid(
                "campaign task issue numbers must be positive and unique",
            ));
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &task.dependencies {
            if !dependencies.insert(dependency) {
                return Err(invalid(format!(
                    "campaign task {} repeats dependency {}",
                    task.id, dependency
                )));
            }
            if !prior.contains(dependency) {
                return Err(invalid(format!(
                    "campaign task {} dependency {} must name an earlier task",
                    task.id, dependency
                )));
            }
        }
        match task.kind.as_str() {
            "implementation" => {
                if task.argv.is_some() || task.runtime_max_sec.is_some() {
                    return Err(invalid(format!(
                        "implementation task {} must not carry argv or runtimeMaxSec",
                        task.id
                    )));
                }
                validate_conflict_domains(&task.conflict_domains, manifest.max_parallel > 1)
                    .with_context(|| format!("campaign task {} conflictDomains", task.id))?;
            }
            "checkpoint" => {
                if !task.conflict_domains.is_empty() {
                    return Err(invalid(format!(
                        "checkpoint task {} must not carry conflictDomains",
                        task.id
                    )));
                }
                let argv = task
                    .argv
                    .as_ref()
                    .ok_or_else(|| invalid(format!("checkpoint task {} requires argv", task.id)))?;
                validate_argv(argv, &format!("checkpoint task {} argv", task.id))?;
                if !matches!(task.runtime_max_sec, Some(value) if value > 0) {
                    return Err(invalid(format!(
                        "checkpoint task {} requires a positive runtimeMaxSec",
                        task.id
                    )));
                }
            }
            _ => unreachable!(),
        }
    }
    Ok(())
}

fn validate_agent(agent: &CampaignAgent) -> Result<()> {
    if agent.adapter.is_empty() || agent.adapter.contains('\0') {
        return Err(invalid("campaign agent.adapter must be non-empty"));
    }
    validate_argv(&agent.argv, "campaign agent.argv")?;
    if !matches!(
        agent.priority.as_str(),
        "interrupt" | "high" | "medium" | "low"
    ) {
        return Err(invalid("campaign agent.priority is invalid"));
    }
    if agent.runtime_max_sec == Some(0)
        || agent.approval_policy.as_deref() == Some("")
        || agent.sandbox_policy.as_deref() == Some("")
        || agent.diagnosis_sandbox_policy.as_deref() == Some("")
        || agent.model.as_deref() == Some("")
    {
        return Err(invalid(
            "campaign agent limits and policy names must be non-empty",
        ));
    }
    Ok(())
}

fn validate_gates(gates: &[CampaignGate]) -> Result<()> {
    if gates.is_empty() || gates.len() > 16 {
        return Err(invalid("campaign gates must contain 1..=16 entries"));
    }
    let mut identifiers = BTreeSet::new();
    for gate in gates {
        if !safe_component(gate.id()) || !identifiers.insert(gate.id()) {
            return Err(invalid("campaign gate ids must be safe and unique"));
        }
        match gate {
            CampaignGate::Command {
                preflight_argv,
                argv,
                runtime_max_sec,
                ..
            } => {
                validate_argv(preflight_argv, "campaign gate preflightArgv")?;
                validate_argv(argv, "campaign gate argv")?;
                if *runtime_max_sec == 0 {
                    return Err(invalid("campaign gate runtimeMaxSec must be positive"));
                }
            }
            CampaignGate::ForbidPaths {
                forbid_paths,
                runtime_max_sec,
                ..
            } => {
                if forbid_paths.is_empty() || forbid_paths.len() > 128 || *runtime_max_sec == 0 {
                    return Err(invalid(
                        "forbidPaths gates require 1..=128 patterns and a positive runtimeMaxSec",
                    ));
                }
                let mut patterns = BTreeSet::new();
                for pattern in forbid_paths {
                    let components = pattern.split('/').collect::<Vec<_>>();
                    if pattern.is_empty()
                        || pattern.len() > 1024
                        || pattern.starts_with('/')
                        || pattern.contains('\0')
                        || components.contains(&"..")
                        || components
                            .iter()
                            .any(|component| component.contains("**") && *component != "**")
                        || !patterns.insert(pattern)
                    {
                        return Err(invalid("campaign forbidPaths patterns are invalid"));
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_argv(argv: &[String], context: &str) -> Result<()> {
    if argv.is_empty()
        || argv
            .iter()
            .any(|argument| argument.is_empty() || argument.chars().any(char::is_control))
    {
        return Err(invalid(format!(
            "{context} must be a non-empty direct argv of non-empty strings without control characters"
        )));
    }
    Ok(())
}

fn validate_conflict_domains(domains: &[String], required: bool) -> Result<()> {
    if required && domains.is_empty() {
        return Err(invalid(
            "must be non-empty when campaign maxParallel is greater than one",
        ));
    }
    let mut seen = BTreeSet::new();
    for domain in domains {
        let path = Path::new(domain);
        if domain.is_empty()
            || domain.ends_with('/')
            || path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
            || !seen.insert(domain)
        {
            return Err(invalid("must contain unique normalized relative paths"));
        }
    }
    Ok(())
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
    let manifest = parse_manifest(master.body.as_deref().unwrap_or_default())?;
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
    // This is the admitted executable graph. Managed checkbox state, issue
    // open/closed projection, update timestamps, and master prose outside the
    // fenced manifest are deliberately excluded. The Python reconciler
    // reconstructs this exact value and refuses any digest mismatch.
    let digest_value = json!({
        "manifest": &manifest,
        "tasks": tasks.iter().map(|issue| json!({
            "number": issue.number,
            "title": issue.title,
            "body": issue.body.as_deref().unwrap_or_default(),
        })).collect::<Vec<_>>(),
    });
    let digest = sha256_json(&digest_value)?;
    Ok(CampaignGraph {
        locator: locator.clone(),
        manifest,
        master,
        tasks,
        executable_digest: digest,
    })
}

fn sha256_json(value: &Value) -> Result<String> {
    fn write(value: &Value, output: &mut String) -> Result<()> {
        match value {
            Value::Null => output.push_str("null"),
            Value::Bool(value) => output.push_str(if *value { "true" } else { "false" }),
            Value::Number(value) => output.push_str(&value.to_string()),
            Value::String(value) => output.push_str(&serde_json::to_string(value)?),
            Value::Array(values) => {
                output.push('[');
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    write(value, output)?;
                }
                output.push(']');
            }
            Value::Object(values) => {
                output.push('{');
                let mut keys = values.keys().collect::<Vec<_>>();
                keys.sort();
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    output.push_str(&serde_json::to_string(key)?);
                    output.push(':');
                    write(&values[key], output)?;
                }
                output.push('}');
            }
        }
        Ok(())
    }

    let mut canonical = String::new();
    write(value, &mut canonical)?;
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

/// The half of the observation that the poll's two REST reads already paid for.
///
/// Every surface the expensive GraphQL walk reads hangs off one of these
/// items: a comment posted or edited on the master or on a task sub-issue, a
/// sub-issue closing behind a merged pull request, an edit to the master body.
/// Each of those moves that item's `updated_at` or `state`, so an unchanged
/// digest here means the walk would return what it returned last time. It is
/// therefore a sound precondition for skipping the walk — and skipping it is
/// what makes a short poll interval genuinely cheap, which is what the
/// interval's own documentation has always claimed.
fn forge_observation(graph: &CampaignGraph, arm_serial: u64) -> Result<String> {
    sha256_json(&json!({
        "graph": graph.executable_digest,
        "forgeState": forge_state_value(graph),
        "armSerial": arm_serial,
    }))
}

fn campaign_observation(
    graph: &CampaignGraph,
    steering: &CampaignSteering,
    arm_serial: u64,
) -> Result<String> {
    sha256_json(&json!({
        "graph": graph.executable_digest,
        "forgeState": forge_state_value(graph),
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
        "examples/flows/spec_build_driver.py",
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

fn validate_host(
    graph: &CampaignGraph,
    config_path: Option<&Path>,
    flow: &Path,
    driver: &Path,
    allow_test_local_forge: bool,
) -> Result<()> {
    let config = load_client_config(config_path)?;
    let required_nodes = max_flow_nodes(&graph.manifest);
    if config.enqueue.fanout_cap < required_nodes {
        return Err(invalid(format!(
            "campaign pass requires enqueue.fanoutCap >= {required_nodes}; host has {}",
            config.enqueue.fanout_cap
        )));
    }
    for pool in [
        "flow",
        "campaign-agent",
        "campaign-control",
        graph.manifest.pool.as_str(),
    ] {
        if !config.pools.contains_key(pool) {
            return Err(invalid(format!(
                "forge-native campaigns require configured pool {pool:?}"
            )));
        }
    }
    let runner = &config.pools[&graph.manifest.pool];
    if runner.resource() != ResourceKind::Mutex || runner.capacity != 1 {
        return Err(invalid(format!(
            "campaign runner pool {:?} must be a capacity-1 mutex",
            graph.manifest.pool
        )));
    }
    let mut required_adapters = vec![
        "shell",
        "spec-build-driver",
        graph.manifest.agent.adapter.as_str(),
    ];
    // The steward is bound as a catalog role, so arming refuses a campaign
    // whose narrator names an adapter this host does not configure rather than
    // degrading every publication to the template at run time.
    if let Some(steward) = &graph.manifest.steward {
        required_adapters.push(steward.adapter.as_str());
    }
    for adapter in required_adapters {
        if !config.adapters.contains_key(adapter) {
            return Err(invalid(format!(
                "forge-native campaigns require configured adapter {adapter:?}"
            )));
        }
    }
    validate_agent_policies(
        &graph.manifest.agent,
        &config.adapters[&graph.manifest.agent.adapter],
    )?;
    if !flow.is_file() || !driver.is_file() {
        return Err(invalid(
            "campaign flow and driver assets must be regular files",
        ));
    }
    let checkout = &graph.manifest.repository.checkout;
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
    if graph.manifest.repository.forge == "local" {
        if !allow_test_local_forge {
            return Err(invalid(
                "forge=local is test-only for issue campaigns; pass --allow-test-local-forge only in an isolated mechanism test",
            ));
        }
    } else {
        let remote = ProcessCommand::new("git")
            .arg("-C")
            .arg(checkout)
            .args([
                "remote",
                "get-url",
                graph.manifest.repository.remote.as_str(),
            ])
            .output()
            .context("cannot execute git while binding campaign remote")?;
        if !remote.status.success() {
            bail!(
                "cannot resolve campaign remote {:?}: {}",
                graph.manifest.repository.remote,
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
    Ok(())
}

fn registry_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("campaigns/armed")
}

struct RegistryLock(fs::File);

impl RegistryLock {
    fn acquire(state_dir: &Path, exclusive: bool) -> Result<Self> {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        let directory = registry_dir(state_dir);
        fs::create_dir_all(&directory)?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(directory.join("registry.lock"))?;
        if exclusive {
            FileExt::lock_exclusive(&file)?;
        } else {
            FileExt::lock_shared(&file)?;
        }
        Ok(Self(file))
    }
}

impl Drop for RegistryLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

fn registration_path(state_dir: &Path, issue_url: &str) -> PathBuf {
    let digest = Sha256::digest(issue_url.as_bytes());
    registry_dir(state_dir).join(format!("{:x}.json", digest))
}

fn write_registration(state_dir: &Path, registration: &CampaignRegistration) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let directory = registry_dir(state_dir);
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let path = registration_path(state_dir, &registration.issue_url);
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(registration)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)?;
    use std::io::Write as _;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

fn read_registration(path: &Path) -> Result<CampaignRegistration> {
    let bytes = fs::read(path)?;
    let registration: CampaignRegistration = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid campaign registration {}", path.display()))?;
    if registration.schema_version != REGISTRY_SCHEMA_VERSION
        || uuid::Uuid::parse_str(&registration.registration_id).is_err()
        || registration.issue_number == 0
        || registration.arm_serial == 0
        || !registration
            .approved_graph_digest
            .strip_prefix("sha256:")
            .is_some_and(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        || !safe_github_login(&registration.authenticated_actor)
        || registration.allowed_actors.is_empty()
        || registration
            .allowed_actors
            .iter()
            .any(|actor| !safe_github_login(actor) || actor != &actor.to_ascii_lowercase())
        || !registration
            .allowed_actors
            .contains(&registration.authenticated_actor)
        || !registration.flow.is_absolute()
        || !registration.driver.is_absolute()
        || !registration.workspace_root.is_absolute()
    {
        return Err(invalid(format!(
            "campaign registration {} violates schema v2; explicitly disarm and re-arm legacy registrations",
            path.display()
        )));
    }
    let locator = parse_issue_url(&registration.issue_url)?;
    if locator.repository != registration.repository || locator.number != registration.issue_number
    {
        return Err(invalid(format!(
            "campaign registration {} has inconsistent locator fields",
            path.display()
        )));
    }
    Ok(registration)
}

fn registrations(state_dir: &Path) -> Result<Vec<(PathBuf, CampaignRegistration)>> {
    let directory = registry_dir(state_dir);
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut paths = fs::read_dir(&directory)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension() == Some(OsStr::new("json")))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| read_registration(&path).map(|registration| (path, registration)))
        .collect()
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
    // implementation lane: prep, agent, ownership, gates, publish, rebase,
    // optional re-gates, merge, then the failure path's machinery retry, diff,
    // diagnosis, and steering, and finally cleanup. A lane that fails at merge
    // is the expensive one, not a lane that merges: maxNodes counts cumulative
    // rows, so finished nodes never return budget. Budgeting the success path
    // alone starves failure steering exactly when it is needed. A machinery
    // fault whose retry budget is already spent records the retry node and is
    // then steered, so both failure paths can land in one lane. Checkpoint
    // lanes are smaller.
    (3 + preflight + manifest.max_parallel * (11 + 2 * manifest.gates.len())) as u32
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
    fn continuation_argv(&self, executable: &Path) -> Vec<String> {
        let mut argv = vec![executable.display().to_string()];
        if let Some(config) = self.config_path {
            argv.push("--config".to_owned());
            argv.push(config.display().to_string());
        }
        argv.extend([
            "--socket".to_owned(),
            self.socket.display().to_string(),
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
    registration: &mut CampaignRegistration,
    wait: bool,
) -> Result<Value> {
    let CampaignHost {
        socket,
        config_path,
        rpc_timeout,
        ..
    } = host;
    if graph.executable_digest != registration.approved_graph_digest {
        bail!(
            "campaign executable graph changed from admitted {} to {}; inspect the issue graph and run `tally campaign arm {}` to approve it",
            registration.approved_graph_digest,
            graph.executable_digest,
            registration.issue_url
        );
    }
    validate_host(
        graph,
        config_path,
        &registration.flow,
        &registration.driver,
        registration.allow_test_local_forge,
    )?;
    let revision = campaign_observation(graph, steering, registration.arm_serial)?;
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
        // The arm CLI's canonical manifest, carried so a reconcile digest
        // mismatch can name its first divergent canonical path (#433). This
        // dispatch only runs because the graph's Rust digest still equals the
        // armed digest, so this value IS the armed manifest. It is evidence
        // for the receipt, never part of the executable graph digest.
        "armedManifest": &graph.manifest,
        "steering": steering.master,
        "taskSteering": steering.tasks,
        "capabilities": {"subIssueWalk": registration.sub_issue_walk},
        "workspaceRoot": &registration.workspace_root,
        "tally": &executable,
        "driver": &registration.driver,
        "driverRuntimeMaxSec": graph.manifest.driver_runtime_max_sec,
        "continuation": {
            "argv": host.continuation_argv(&executable),
            // The control pool, not the campaign mutex: the scan must be free
            // to run while this pass finishes its cleanup. Its dispatch still
            // queues behind the capacity-1 runner mutex, so passes serialize.
            "pool": ["campaign-control"],
            "priority": "low",
            "runtimeMaxSec": graph.manifest.driver_runtime_max_sec,
            "eventsDir": host.events_dir(),
        },
    });
    let mut flow_argv = vec![
        executable.display().to_string(),
        "flow".to_owned(),
        "run".to_owned(),
        registration.flow.display().to_string(),
        "--args-from-brief".to_owned(),
        "--max-nodes".to_owned(),
        max_flow_nodes(&graph.manifest).to_string(),
    ];
    // #432: the projection wait reaches the pass on its argv, not through the
    // environment. This pass runs as a daemon-launched transient unit whose
    // environment is an explicit `--setenv` list, so nothing an operator
    // exports at arm time is visible to it. Absent leaves the argv exactly as
    // it was, so a campaign armed without the flag dispatches byte-identically
    // to before.
    if let Some(millis) = registration.projection_wait_ms {
        flow_argv.push("--result-projection-wait-ms".to_owned());
        flow_argv.push(millis.to_string());
    }
    let payload = EnqueuePayload {
        invocation: None,
        argv: Some(flow_argv),
        pools: Some(vec!["flow".to_owned(), graph.manifest.pool.clone()]),
        executor: None,
        priority: Some(priority(&graph.manifest.agent.priority)),
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
            "revision": &revision,
            "approvedBy": &registration.authenticated_actor,
            "allowedActors": &registration.allowed_actors,
            "graphDigest": &registration.approved_graph_digest,
        })),
        manifest_hash: Some(graph.executable_digest.clone()),
        consumption_estimate: None,
        runtime_max_sec: graph.manifest.runtime_max_sec,
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
    let locator = parse_issue_url(&args.issue)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let _lock = RegistryLock::acquire(&state_dir, true)?;
    let authenticated_actor = authenticated_actor()?;
    let allowed_actors = normalize_allowed_actors(&args.allowed_actors, &authenticated_actor)?;
    let graph = fetch_campaign_graph(&locator)?;
    require_allowed_issue_authors(&graph, &allowed_actors)?;
    // Probe once, at arm, and record the answer. A pass never has to discover
    // mid-flight that half its projection is unavailable.
    let sub_issue_walk = probe_sub_issue_walk(&locator)?;
    let steering = fetch_campaign_steering(&graph, &allowed_actors, sub_issue_walk)?;
    let path = registration_path(&state_dir, &locator.url);
    let prior = if path.exists() {
        Some(read_registration(&path)?)
    } else {
        None
    };
    let flow = resolve_flow(args.flow)?;
    let driver = resolve_driver(args.driver)?;
    let workspace_root = args
        .workspace_root
        .map_or_else(default_campaign_workspace_root, Ok)?;
    if !workspace_root.is_absolute() {
        return Err(invalid("campaign workspace root must be absolute"));
    }
    // #432: refused at arm rather than at the first pass, because the value is
    // durable — a zero here would be recorded and then rejected by every
    // `flow run` this campaign ever dispatches, including the ones the poll
    // timer dispatches unattended.
    if args.projection_wait_ms == Some(0) {
        return Err(invalid("--projection-wait-ms must be greater than zero"));
    }
    let arm_serial = prior.as_ref().map_or(Ok(1), |value| {
        value
            .arm_serial
            .checked_add(1)
            .ok_or_else(|| invalid("campaign arm retry counter is exhausted"))
    })?;
    let mut registration = CampaignRegistration {
        schema_version: REGISTRY_SCHEMA_VERSION,
        registration_id: prior.as_ref().map_or_else(
            || uuid::Uuid::now_v7().to_string(),
            |value| value.registration_id.clone(),
        ),
        issue_url: locator.url.clone(),
        repository: locator.repository.clone(),
        issue_number: locator.number,
        armed_at: Utc::now().to_rfc3339(),
        arm_serial,
        approved_graph_digest: graph.executable_digest.clone(),
        authenticated_actor,
        allowed_actors,
        allow_test_local_forge: args.allow_test_local_forge,
        sub_issue_walk,
        last_observation: prior.and_then(|value| value.last_observation),
        // Arming always dispatches, so the cheap precondition starts empty and
        // the first poll after an arm re-establishes it.
        last_forge_observation: None,
        projection_wait_ms: args.projection_wait_ms,
        flow,
        driver,
        workspace_root,
    };
    validate_host(
        &graph,
        config_path,
        &registration.flow,
        &registration.driver,
        registration.allow_test_local_forge,
    )?;
    write_registration(&state_dir, &registration)?;
    if args.no_enqueue {
        outln!(
            "{}",
            serde_json::to_string(&json!({
                "status": "armed",
                "issue": locator.url,
                "tasks": graph.tasks.len(),
                "graphDigest": graph.executable_digest,
                "allowedActors": registration.allowed_actors,
                "subIssueWalk": registration.sub_issue_walk,
                "projection": projection_label(registration.sub_issue_walk),
                "enqueued": false,
            }))?
        );
        return Ok(());
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
        &mut registration,
        args.wait,
    )
    .await?;
    write_registration(&state_dir, &registration)?;
    outln!(
        "{}",
        serde_json::to_string(&armed_projection(&result, registration.sub_issue_walk))?
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

async fn run_campaign_poll(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: CampaignPollArgs,
) -> Result<()> {
    if !args.once {
        return Err(invalid("campaign poll currently requires --once"));
    }
    let state_dir = resolve_state_dir(args.state_dir)?;
    let _lock = RegistryLock::acquire(&state_dir, true)?;
    let entries = registrations(&state_dir)?;
    let mut observed = 0usize;
    let mut dispatched = 0usize;
    let mut pruned = 0usize;
    let mut failures = Vec::new();
    for (path, mut registration) in entries {
        observed += 1;
        let attempt = async {
            let locator = parse_issue_url(&registration.issue_url)?;
            require_authenticated_actor(&registration)?;
            let master = fetch_issue(&locator)?;
            if master.state != "open" {
                fs::remove_file(&path)?;
                return Ok((false, true));
            }
            let graph = campaign_graph_from(&locator, master)?;
            require_allowed_issue_authors(&graph, &registration.allowed_actors)?;
            if graph.executable_digest != registration.approved_graph_digest {
                bail!(
                    "executable graph changed from admitted {} to {}; explicit re-arm is required",
                    registration.approved_graph_digest,
                    graph.executable_digest
                );
            }
            // The cheap comparison comes first. Reading the steering surfaces
            // runs the full bounded GraphQL walk over every sub-issue thread,
            // and running it before deciding whether anything moved made every
            // tick of a 60 s timer pay for a traversal that almost always
            // returned what the previous tick returned.
            let forge = forge_observation(&graph, registration.arm_serial)?;
            if registration.last_forge_observation.as_deref() == Some(&forge) {
                return Ok((false, false));
            }
            let steering = fetch_campaign_steering(
                &graph,
                &registration.allowed_actors,
                registration.sub_issue_walk,
            )?;
            let observation = campaign_observation(&graph, &steering, registration.arm_serial)?;
            if registration.last_observation.as_deref() == Some(&observation) {
                registration.last_forge_observation = Some(forge);
                write_registration(&state_dir, &registration)?;
                return Ok((false, false));
            }
            registration.last_forge_observation = Some(forge);
            let result = dispatch_campaign(
                CampaignHost {
                    socket,
                    config_path,
                    state_dir: &state_dir,
                    rpc_timeout,
                },
                &graph,
                &steering,
                &mut registration,
                args.wait,
            )
            .await?;
            write_registration(&state_dir, &registration)?;
            if args.wait {
                let code = waited_exit_code(&result);
                if code != 0 {
                    return Err(anyhow::Error::new(ExitFailure {
                        code,
                        message: format!(
                            "campaign reconcile pass for {} returned a non-zero verdict",
                            registration.issue_url
                        ),
                    }));
                }
            }
            Ok::<_, anyhow::Error>((true, false))
        }
        .await;
        match attempt {
            Ok((true, _)) => dispatched += 1,
            Ok((_, true)) => pruned += 1,
            Ok((false, false)) => {}
            Err(error) => failures.push(format!("{}: {error:#}", path.display())),
        }
    }
    outln!(
        "{}",
        serde_json::to_string(&json!({
            "observed": observed,
            "dispatched": dispatched,
            "pruned": pruned,
            "failures": failures,
        }))?
    );
    if failures.is_empty() {
        Ok(())
    } else {
        bail!("one or more armed campaigns could not be polled")
    }
}

fn run_campaign_list(args: CampaignListArgs) -> Result<()> {
    let state_dir = resolve_state_dir(args.state_dir)?;
    let _lock = RegistryLock::acquire(&state_dir, false)?;
    let values = registrations(&state_dir)?
        .into_iter()
        .map(|(_, registration)| registration)
        .collect::<Vec<_>>();
    outln!("{}", serde_json::to_string(&values)?);
    Ok(())
}

fn run_campaign_disarm(args: CampaignDisarmArgs) -> Result<()> {
    let locator = parse_issue_url(&args.issue)?;
    let state_dir = resolve_state_dir(args.state_dir)?;
    let _lock = RegistryLock::acquire(&state_dir, true)?;
    let path = registration_path(&state_dir, &locator.url);
    let removed = if path.exists() {
        fs::remove_file(&path)?;
        true
    } else {
        false
    };
    outln!(
        "{}",
        serde_json::to_string(&json!({"issue": locator.url, "disarmed": removed}))?
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
) -> Result<String> {
    if let Some(body) = object.get("body") {
        return body
            .as_str()
            .filter(|body| !body.trim().is_empty() && !body.contains('\0'))
            .map(ToOwned::to_owned)
            .ok_or_else(|| invalid(format!("{context}.body must be a non-empty string")));
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

fn project_tasks(document: &Value) -> Result<Vec<ProjectTask>> {
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
            project_string_list(
                item.get("conflictDomains"),
                &format!("{context}.conflictDomains"),
            )?
        } else {
            Vec::new()
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
        let body = render_project_task_body(item, &context)?;
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
                    "conflictDomains": task.conflict_domains,
                });
                if task.kind == "checkpoint" {
                    let object = reference.as_object_mut().expect("reference is an object");
                    object.remove("conflictDomains");
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
            let legacy = legacy_checkpoint_tag_prefix(
                &manifest.name,
                issue_number,
                &task.id,
                &graph_digest,
            )?;
            if projected_checkpoint_complete(manifest, &reference)?
                || projected_checkpoint_complete(manifest, &legacy)?
            {
                merged.insert(task.id.clone());
            }
            continue;
        }
        let revision = sha256_json(&json!({
            "source": graph_digest,
            "task": task.id,
        }))?;
        let marker = format!(
            "<!-- tally:spec-build:v2 campaign={} issue={} task={} revision={} -->",
            manifest.name, issue_number, task.id, revision
        );
        let branch = stable_publish_branch(&manifest.name, issue_number, &task.id, Some(&revision));
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
/// revision that was. Reading the exact prefix as if it were the ref name is
/// how both namespaces silently matched nothing at all. Every receipt under
/// the family is considered and one that the base branch contains is enough;
/// the driver's own exact-revision check remains the completion oracle, and
/// this is the checkbox the reader sees.
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
    revision: Option<&str>,
) -> String {
    let slug = campaign.trim_matches(['.', '-']);
    let slug = &slug[..slug.len().min(32)];
    let suffix = revision
        .and_then(|value| value.strip_prefix("sha256:"))
        .map(|value| format!("-{}", &value[..value.len().min(16)]))
        .unwrap_or_default();
    format!("tally/{slug}-issue-{issue_number}/{task_id}{suffix}")
}

/// The family of refs one checkpoint's receipts are published under.
///
/// The driver appends `/<baseRevision>` to this prefix for the revision it
/// actually tested (`checkpoint_ref` in `spec_build_driver.py`); the shared
/// vectors in `test/fixtures/spec-build/checkpoint-refs.json` pin the two
/// layouts together from both languages.
///
/// The pre-#307 namespace was `refs/tags/`, which every clone of a public
/// target repository auto-fetches; a campaign's checkpoint ledger became part
/// of that repository's public surface. Receipts now share the hidden
/// namespace the campaign's other durable state already uses.
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
    let scope = format!(
        "{:x}",
        Sha256::digest(format!("{campaign}\0{issue_number}").as_bytes())
    );
    Ok(format!(
        "refs/tally/spec-build/v1/{}/checkpoint/{task_id}-{digest}",
        &scope[..24],
    ))
}

/// The visible tag family published before the namespace moved. Read for
/// compatibility so an existing campaign is never re-executed; never written.
/// Like the hidden namespace above this is a prefix: the tested base revision
/// is the last path component.
fn legacy_checkpoint_tag_prefix(
    campaign: &str,
    issue_number: u64,
    task_id: &str,
    source: &str,
) -> Result<String> {
    let digest = source
        .strip_prefix("sha256:")
        .filter(|value| value.len() == 64)
        .ok_or_else(|| invalid("checkpoint source is not a SHA-256 identity"))?;
    let mut readable = campaign
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    while readable.contains("--") {
        readable = readable.replace("--", "-");
    }
    readable = readable.trim_matches('-').chars().take(24).collect();
    readable = readable.trim_end_matches('-').to_owned();
    if readable.is_empty() {
        readable = "campaign".to_owned();
    }
    let campaign_identity = format!("{:x}", Sha256::digest(campaign.as_bytes()));
    Ok(format!(
        "refs/tags/tally/spec-build/v1/{}-{}-issue-{issue_number}/{task_id}-{digest}",
        readable,
        &campaign_identity[..12],
    ))
}

fn validate_project_shape(config: &Value, tasks: &[ProjectTask]) -> Result<()> {
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
    let manifest: CampaignManifest = serde_json::from_value(value).map_err(|error| {
        invalid(format!(
            "campaign configuration cannot form a valid manifest: {error}"
        ))
    })?;
    validate_manifest(&manifest)
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
    let document = read_json_document(&args.worklist, "campaign worklist")?;
    let separate = args
        .campaign_config
        .as_deref()
        .map(|path| read_json_document(path, "campaign configuration"))
        .transpose()?;
    let config = project_config(&document, separate.as_ref())?;
    let mut tasks = project_tasks(&document)?;
    validate_project_shape(&config, &tasks)?;
    let name = config
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| safe_component(value))
        .ok_or_else(|| invalid("campaign configuration name is missing or invalid"))?;
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
    let manifest: CampaignManifest = serde_json::from_value(final_value).map_err(|error| {
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
    outln!(
        "{}",
        serde_json::to_string(&json!({
            "issue": locator.url,
            "tasks": tasks.iter().map(|task| json!({"id": task.id, "issue": task.issue})).collect::<Vec<_>>(),
            "merged": merged,
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
            let legacy =
                legacy_checkpoint_tag_prefix(campaign, issue_number, task_id, source).unwrap();
            assert_eq!(legacy, vector["legacyTagPrefix"].as_str().unwrap());

            // The prefix has to be exactly the driver's ref minus the tested
            // revision, or the `<prefix>/*` query the projection runs matches
            // nothing the driver ever published.
            assert_eq!(
                vector["ref"].as_str().unwrap(),
                format!("{prefix}/{base_revision}")
            );
            assert_eq!(
                vector["legacyTag"].as_str().unwrap(),
                format!("{legacy}/{base_revision}")
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
    fn a_registration_written_before_the_probe_reads_as_degraded() {
        let registration: CampaignRegistration = serde_json::from_value(json!({
            "schemaVersion": REGISTRY_SCHEMA_VERSION,
            "registrationId": "0198f000-0000-7000-8000-000000000001",
            "issueUrl": "https://github.com/acme/widgets/issues/42",
            "repository": "acme/widgets",
            "issueNumber": 42,
            "armedAt": "2026-08-01T10:00:00Z",
            "armSerial": 1,
            "approvedGraphDigest": format!("sha256:{}", "a".repeat(64)),
            "authenticatedActor": "operator",
            "allowedActors": ["operator"],
            "allowTestLocalForge": false,
            "flow": "/nix/store/spec-build.js",
            "driver": "/nix/store/spec_build_driver.py",
            "workspaceRoot": "/var/lib/tally/campaigns",
        }))
        .unwrap();
        assert!(!registration.sub_issue_walk);
    }

    #[test]
    fn a_task_thread_comment_moves_the_observation_revision() {
        let graph = CampaignGraph {
            locator: parse_issue_url("https://github.com/acme/widgets/issues/42").unwrap(),
            manifest: serde_json::from_value(manifest_value_for_test(json!([{
                "id": "foundation",
                "kind": "implementation",
                "issue": 43,
                "dependencies": [],
                "conflictDomains": []
            }])))
            .unwrap(),
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
            executable_digest: format!("sha256:{}", "a".repeat(64)),
        };
        let quiet = CampaignSteering::default();
        let steered = CampaignSteering {
            master: Vec::new(),
            tasks: BTreeMap::from([("43".to_owned(), vec![json!({"body": "rerun it"})])]),
        };
        assert_ne!(
            campaign_observation(&graph, &quiet, 1).unwrap(),
            campaign_observation(&graph, &steered, 1).unwrap()
        );
    }

    /// The poll skips the expensive sub-issue walk while this digest holds
    /// still, so anything the walk could see has to move it.
    #[test]
    fn the_cheap_poll_precondition_moves_with_every_surface_the_walk_reads() {
        let base = graph_for_forge_observation();
        let unchanged = graph_for_forge_observation();
        assert_eq!(
            forge_observation(&base, 1).unwrap(),
            forge_observation(&unchanged, 1).unwrap()
        );
        let quiet = forge_observation(&base, 1).unwrap();

        // A comment on the master thread bumps the master's updated_at.
        let mut master_touched = graph_for_forge_observation();
        master_touched.master.updated_at = "2026-08-01T11:00:00Z".to_owned();
        assert_ne!(forge_observation(&master_touched, 1).unwrap(), quiet);

        // A comment on a task's own sub-issue bumps that sub-issue's.
        let mut task_touched = graph_for_forge_observation();
        task_touched.tasks[0].updated_at = "2026-08-01T11:00:00Z".to_owned();
        assert_ne!(forge_observation(&task_touched, 1).unwrap(), quiet);

        // A merged pull request closing a sub-issue changes its state.
        let mut task_closed = graph_for_forge_observation();
        task_closed.tasks[0].state = "closed".to_owned();
        assert_ne!(forge_observation(&task_closed, 1).unwrap(), quiet);

        // Re-arming always dispatches, so it must invalidate the precondition.
        assert_ne!(forge_observation(&base, 2).unwrap(), quiet);
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
            manifest: serde_json::from_value(manifest_value_for_test(json!([{
                "id": "foundation",
                "kind": "implementation",
                "issue": 43,
                "dependencies": [],
                "conflictDomains": []
            }])))
            .unwrap(),
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
            executable_digest: format!("sha256:{}", "a".repeat(64)),
        }
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
        let tasks = project_tasks(&document).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].dependencies, ["foundation"]);
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
        assert!(project_tasks(&document).is_err());
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
    fn project_shape_is_validated_before_projection() {
        let document = json!({
            "schemaVersion": 1,
            "tasks": [
                {"id": "one", "kind": "implementation", "title": "One", "body": "First.", "dependencies": [], "conflictDomains": []},
                {"id": "two", "kind": "implementation", "title": "Two", "body": "Second.", "dependencies": ["one"], "conflictDomains": []}
            ]
        });
        let mut config = manifest_value_for_test(json!([]));
        config.as_object_mut().unwrap().remove("tasks");
        config
            .as_object_mut()
            .unwrap()
            .insert("maxTasks".into(), json!(1));
        assert!(validate_project_shape(&config, &project_tasks(&document).unwrap()).is_err());
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
        // Byte-for-byte the invocation tally-campaign-poll.service runs, so a
        // continuation event and a timer firing produce the same observation
        // revision and therefore the same dispatch dedup key.
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
            without_config.continuation_argv(Path::new("/nix/store/tally/bin/tally"))[1],
            "--socket"
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
        // asserted to be 52 too; change one side and the other must follow.
        // 3 + (2 + 2*1) + 3*(11 + 2*2) = 52.
        assert_eq!(max_flow_nodes(&manifest), 52);
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
        const LANE_SUCCESS_PATH: usize = 7;
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
        assert_eq!(
            steward.final_message_pattern.as_deref(),
            Some("^NARRATOR_RESULT=(.*)$")
        );

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
        assert!(error.contains("steward argv must be non-empty"), "{error}");
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
            error.contains("agent limits and policy names must be non-empty"),
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

    /// The packaged driver and this CLI must agree on the canonical campaign
    /// agent byte-for-byte, because each digests it independently and the
    /// reconcile node refuses any mismatch. #429: the driver's `forge_manifest`
    /// unconditionally normalized an 8th agent field (`diagnosisSandboxPolicy`)
    /// that this struct did not carry, so no manifest could make the two
    /// digests agree and every forge-native arm failed reconcile. This test
    /// computes the graph digest through BOTH halves — the Rust `sha256_json`
    /// path here, and the real `spec_build_driver.py` `canonical_sha256` path
    /// run as a `python3` subprocess (the packaged file, not a copy of its
    /// logic) — and asserts byte equality, so a future version skew inside a
    /// pin fails in CI instead of at first arm.
    ///
    /// Two manifests, because a schema has two ways to skew. The first carries
    /// every optional field explicitly, which catches a field one half omits
    /// or renames. The second carries only the required ones, which catches a
    /// field whose two halves disagree on the DEFAULT — the shape an ad-hoc
    /// campaign manifest actually has, and the shape dotfiles#163 arm'd under.
    #[test]
    fn graph_digest_is_byte_identical_between_the_cli_and_the_packaged_driver() {
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let driver = repo_root.join("examples/flows/spec_build_driver.py");
        assert!(
            driver.is_file(),
            "packaged driver missing: {}",
            driver.display()
        );

        // `forge_manifest` validates `repository.checkout` as an existing Git
        // directory, so the fixture needs a real one. Canonicalize it so the
        // driver's `Path.resolve()` cannot rewrite it into a different digest.
        let checkout_dir = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(checkout_dir.path())
            .status()
            .expect("git init must run for the schema-parity fixture");
        assert!(
            status.success(),
            "git init failed for the schema-parity fixture"
        );
        let checkout = fs::canonicalize(checkout_dir.path()).unwrap();
        let checkout = checkout.to_str().expect("checkout path must be UTF-8");

        // The packaged driver's digest path, run once per fixture: load the
        // real file under python3, normalize the manifest through the driver's
        // own `forge_manifest`, and hash it with the driver's own
        // `canonical_sha256`.
        let driver_digest = |manifest: &Value, tasks: &Value| -> String {
            let input_dir = tempfile::tempdir().unwrap();
            let input_path = input_dir.path().join("parity-graph.json");
            fs::write(
                &input_path,
                serde_json::to_string(&json!({"manifest": manifest, "tasks": tasks})).unwrap(),
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
_, _, normalized_manifest = module.forge_manifest(data["manifest"])
source = {"manifest": normalized_manifest, "tasks": data["tasks"]}
sys.stdout.write(module.canonical_sha256(source))
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
            String::from_utf8(output.stdout)
                .expect("driver parity probe must print UTF-8")
                .trim()
                .to_owned()
        };

        // The arm CLI's digest path: parse the manifest, canonicalize, hash.
        let cli_digest = |manifest: &Value, tasks: &Value| -> String {
            let parsed: CampaignManifest = serde_json::from_value(manifest.clone())
                .expect("the parity manifest must parse as a CampaignManifest");
            validate_manifest(&parsed).expect("the parity manifest must validate");
            sha256_json(&json!({"manifest": &parsed, "tasks": tasks}))
                .expect("sha256_json must succeed on the parity graph")
        };

        // One manifest carrying every optional field at every level.
        let manifest = json!({
            "schemaVersion": 1,
            "name": "parity",
            "repository": {
                "checkout": checkout,
                "baseBranch": "main",
                "remote": "origin",
                "forge": "github"
            },
            "maxTasks": 4,
            "maxParallel": 1,
            "driverRuntimeMaxSec": 900,
            "runtimeMaxSec": 86_400,
            "pool": "campaign",
            "mergeMethod": "squash",
            "gitAiBinding": "off",
            "gitAiAwaitSec": 60,
            "agent": {
                "adapter": "codex",
                "argv": [BRIEF_SENTINEL],
                "priority": "low",
                "runtimeMaxSec": 14_400,
                "approvalPolicy": "never",
                "sandboxPolicy": "danger-full-access",
                "diagnosisSandboxPolicy": "read-only",
                "model": "parity/model"
            },
            "steward": {
                "adapter": "narrator",
                "argv": ["narrate"],
                "env": {"STEWARD_MODE": "narrate"},
                "finalMessagePattern": "^TALLY_FINAL_MESSAGE=(.*)$",
                "runtimeMaxSec": 900
            },
            "gates": [
                {
                    "kind": "command",
                    "id": "gate-command",
                    "preflightArgv": ["true"],
                    "argv": ["true"],
                    "runtimeMaxSec": 900
                },
                {
                    "kind": "forbidPaths",
                    "id": "gate-forbid",
                    "forbidPaths": ["*.db"],
                    "runtimeMaxSec": 900
                }
            ],
            "tasks": [
                {
                    "id": "task-a",
                    "kind": "implementation",
                    "issue": 101,
                    "dependencies": [],
                    "conflictDomains": ["src"]
                },
                {
                    "id": "task-b",
                    "kind": "checkpoint",
                    "issue": 102,
                    "dependencies": ["task-a"],
                    "argv": ["true"],
                    "runtimeMaxSec": 60
                }
            ]
        });
        let tasks = json!([
            {"number": 101, "title": "Implement the thing", "body": "Brief for task-a."},
            {"number": 102, "title": "Checkpoint the thing", "body": "Brief for task-b."}
        ]);

        assert_eq!(
            cli_digest(&manifest, &tasks),
            driver_digest(&manifest, &tasks),
            "the CLI and the packaged driver disagree on the campaign graph digest \
             for a manifest carrying every optional field; a forge-native campaign \
             would fail reconcile with this skew"
        );

        // The same graph with every optional field left out, so each half fills
        // it from its OWN default. This is the shape an ad-hoc campaign manifest
        // has, and it is the one that failed at dotfiles#163: a field the two
        // halves default differently is invisible to the maximal fixture above.
        let defaults = json!({
            "schemaVersion": 1,
            "name": "parity",
            "repository": {"checkout": checkout},
            "agent": {},
            "gates": [{"kind": "forbidPaths", "id": "gate-forbid", "forbidPaths": ["*.db"]}],
            "tasks": [
                {"id": "task-a", "kind": "implementation", "issue": 101}
            ]
        });
        let default_tasks = json!([
            {"number": 101, "title": "Implement the thing", "body": "Brief for task-a."}
        ]);
        assert_eq!(
            cli_digest(&defaults, &default_tasks),
            driver_digest(&defaults, &default_tasks),
            "the CLI and the packaged driver disagree on the campaign graph digest \
             for a manifest that leaves every optional field to each half's own \
             default; a forge-native campaign would fail reconcile with this skew"
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
        let driver = repo_root.join("examples/flows/spec_build_driver.py");
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
    fn registration_v2_round_trips_local_authority() {
        let root = tempfile::tempdir().unwrap();
        let state_dir = root.path();
        let authenticated = "operator".to_owned();
        let registration = CampaignRegistration {
            schema_version: REGISTRY_SCHEMA_VERSION,
            registration_id: uuid::Uuid::now_v7().to_string(),
            issue_url: "https://github.com/acme/widgets/issues/42".to_owned(),
            repository: "acme/widgets".to_owned(),
            issue_number: 42,
            armed_at: "2026-08-01T00:00:00Z".to_owned(),
            arm_serial: 1,
            approved_graph_digest: format!("sha256:{}", "a".repeat(64)),
            authenticated_actor: authenticated.clone(),
            allowed_actors: normalize_allowed_actors(&["Reviewer".into()], &authenticated).unwrap(),
            allow_test_local_forge: false,
            sub_issue_walk: true,
            last_observation: None,
            last_forge_observation: None,
            projection_wait_ms: Some(240_000),
            flow: PathBuf::from("/nix/store/flow.js"),
            driver: PathBuf::from("/nix/store/driver"),
            workspace_root: PathBuf::from("/srv/tally-campaigns"),
        };
        let _lock = RegistryLock::acquire(state_dir, true).unwrap();
        write_registration(state_dir, &registration).unwrap();
        let loaded =
            read_registration(&registration_path(state_dir, &registration.issue_url)).unwrap();
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

    /// #432 acceptance 2, the seam that actually reaches a campaign pass. A
    /// registration written before `--projection-wait-ms` existed carries no
    /// field at all; it must still load, and it must leave the flow host's own
    /// default alone rather than being refused or defaulted to zero.
    #[test]
    fn a_registration_without_a_projection_wait_still_loads() {
        let root = tempfile::tempdir().unwrap();
        let state_dir = root.path();
        let url = "https://github.com/acme/widgets/issues/42";
        let path = registration_path(state_dir, url);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_string(&json!({
                "schemaVersion": REGISTRY_SCHEMA_VERSION,
                "registrationId": uuid::Uuid::now_v7().to_string(),
                "issueUrl": url,
                "repository": "acme/widgets",
                "issueNumber": 42,
                "armedAt": "2026-08-01T00:00:00Z",
                "armSerial": 1,
                "approvedGraphDigest": format!("sha256:{}", "a".repeat(64)),
                "authenticatedActor": "operator",
                "allowedActors": ["operator"],
                "allowTestLocalForge": false,
                "subIssueWalk": true,
                "flow": "/nix/store/flow.js",
                "driver": "/nix/store/driver",
                "workspaceRoot": "/srv/tally-campaigns",
            }))
            .unwrap(),
        )
        .unwrap();
        let loaded = read_registration(&path).unwrap();
        assert_eq!(loaded.projection_wait_ms, None);
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
        CampaignAgent {
            adapter: "codex".to_owned(),
            argv: default_agent_argv(),
            priority: default_agent_priority(),
            runtime_max_sec: default_agent_runtime_max_sec(),
            approval_policy: default_agent_approval_policy(),
            sandbox_policy: sandbox.map(str::to_owned),
            diagnosis_sandbox_policy: default_agent_diagnosis_sandbox_policy(),
            model: None,
        }
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
