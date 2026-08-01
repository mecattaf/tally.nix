use super::*;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::process::{Command as ProcessCommand, Stdio};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
const DEFAULT_AGENT_APPROVAL_POLICY: &str = "on-request";
const DEFAULT_AGENT_SANDBOX_POLICY: &str = "workspace-write";
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
    agent: CampaignAgent,
    gates: Vec<CampaignGate>,
    tasks: Vec<CampaignTaskReference>,
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
    #[serde(default)]
    last_observation: Option<String>,
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

fn run_gh(arguments: &[OsString], stdin: Option<&str>) -> Result<String> {
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
    let output = child.wait_with_output()?;
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
    validate_agent(&manifest.agent)?;
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

fn campaign_observation(
    graph: &CampaignGraph,
    steering: &[Value],
    arm_serial: u64,
) -> Result<String> {
    sha256_json(&json!({
        "graph": graph.executable_digest,
        "forgeState": {
            "master": {
                "state": graph.master.state,
                "updatedAt": graph.master.updated_at,
            },
            "tasks": graph.tasks.iter().map(|issue| json!({
                "number": issue.number,
                "state": issue.state,
                "updatedAt": issue.updated_at,
            })).collect::<Vec<_>>(),
        },
        "steering": steering,
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
    if runner.resource != ResourceKind::Mutex || runner.capacity != 1 {
        return Err(invalid(format!(
            "campaign runner pool {:?} must be a capacity-1 mutex",
            graph.manifest.pool
        )));
    }
    for adapter in [
        "shell",
        "spec-build-driver",
        graph.manifest.agent.adapter.as_str(),
    ] {
        if !config.adapters.contains_key(adapter) {
            return Err(invalid(format!(
                "forge-native campaigns require configured adapter {adapter:?}"
            )));
        }
    }
    let adapter = &config.adapters[&graph.manifest.agent.adapter];
    if let Some(policy) = &graph.manifest.agent.approval_policy {
        if !adapter.launch.approval_policies.contains_key(policy) {
            return Err(invalid(format!(
                "campaign agent approvalPolicy {policy:?} is not authorized by adapter {:?}",
                graph.manifest.agent.adapter
            )));
        }
    }
    if let Some(policy) = &graph.manifest.agent.sandbox_policy {
        if !adapter.launch.sandbox_policies.contains_key(policy) {
            return Err(invalid(format!(
                "campaign agent sandboxPolicy {policy:?} is not authorized by adapter {:?}",
                graph.manifest.agent.adapter
            )));
        }
    }
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
    let preflight = if command_gates == 0 {
        0
    } else {
        command_gates + 2
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

async fn dispatch_campaign(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    graph: &CampaignGraph,
    steering: &[Value],
    registration: &mut CampaignRegistration,
    wait: bool,
) -> Result<Value> {
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
        "steering": steering,
        "workspaceRoot": &registration.workspace_root,
        "tally": &executable,
        "driver": &registration.driver,
        "driverRuntimeMaxSec": graph.manifest.driver_runtime_max_sec,
    });
    let payload = EnqueuePayload {
        invocation: None,
        argv: Some(vec![
            executable.display().to_string(),
            "flow".to_owned(),
            "run".to_owned(),
            registration.flow.display().to_string(),
            "--args-from-brief".to_owned(),
            "--max-nodes".to_owned(),
            max_flow_nodes(&graph.manifest).to_string(),
        ]),
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
    let steering = fetch_steering(&locator, &allowed_actors)?;
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
        last_observation: prior.and_then(|value| value.last_observation),
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
        println!(
            "{}",
            serde_json::to_string(&json!({
                "status": "armed",
                "issue": locator.url,
                "tasks": graph.tasks.len(),
                "graphDigest": graph.executable_digest,
                "allowedActors": registration.allowed_actors,
                "enqueued": false,
            }))?
        );
        return Ok(());
    }
    let result = dispatch_campaign(
        socket,
        config_path,
        rpc_timeout,
        &graph,
        &steering,
        &mut registration,
        args.wait,
    )
    .await?;
    write_registration(&state_dir, &registration)?;
    println!("{}", serde_json::to_string(&result)?);
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
            let graph = fetch_campaign_graph(&locator)?;
            require_allowed_issue_authors(&graph, &registration.allowed_actors)?;
            if graph.executable_digest != registration.approved_graph_digest {
                bail!(
                    "executable graph changed from admitted {} to {}; explicit re-arm is required",
                    registration.approved_graph_digest,
                    graph.executable_digest
                );
            }
            let steering = fetch_steering(&locator, &registration.allowed_actors)?;
            let observation = campaign_observation(&graph, &steering, registration.arm_serial)?;
            if registration.last_observation.as_deref() == Some(&observation) {
                return Ok((false, false));
            }
            let result = dispatch_campaign(
                socket,
                config_path,
                rpc_timeout,
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
    println!(
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
    println!("{}", serde_json::to_string(&values)?);
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
    println!(
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
                checkpoint_reference(&manifest.name, issue_number, &task.id, &graph_digest)?;
            if projected_checkpoint_complete(manifest, &reference)? {
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

fn projected_checkpoint_complete(manifest: &CampaignManifest, reference: &str) -> Result<bool> {
    let git = |arguments: &[&str]| -> Result<std::process::Output> {
        ProcessCommand::new("git")
            .arg("-C")
            .arg(&manifest.repository.checkout)
            .args(arguments)
            .output()
            .context("cannot query projected checkpoint completion")
    };
    let listed = git(&[
        "ls-remote",
        "--refs",
        &manifest.repository.remote,
        reference,
    ])?;
    if !listed.status.success() {
        bail!(
            "cannot query checkpoint ref {reference}: {}",
            String::from_utf8_lossy(&listed.stderr).trim()
        );
    }
    let stdout = String::from_utf8(listed.stdout).context("git ls-remote was not UTF-8")?;
    if stdout.trim().is_empty() {
        return Ok(false);
    }
    let lines = stdout.lines().collect::<Vec<_>>();
    if lines.len() != 1 {
        bail!("checkpoint ref {reference} returned more than one remote row");
    }
    let target = lines[0]
        .split_once('\t')
        .filter(|(_, name)| *name == reference)
        .map(|(target, _)| target)
        .filter(|target| {
            (40..=64).contains(&target.len()) && target.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
        .ok_or_else(|| {
            invalid(format!(
                "checkpoint ref {reference} returned malformed output"
            ))
        })?;
    let fetched_base = git(&["fetch", "--prune", "--no-tags", &manifest.repository.remote])?;
    if !fetched_base.status.success() {
        bail!("cannot refresh campaign base while projecting checkpoint state");
    }
    let fetched_tag = git(&["fetch", "--no-tags", &manifest.repository.remote, reference])?;
    if !fetched_tag.status.success() {
        bail!("cannot fetch checkpoint ref {reference}");
    }
    let object_type = git(&["cat-file", "-t", target])?;
    if !object_type.status.success()
        || String::from_utf8_lossy(&object_type.stdout).trim() != "commit"
    {
        return Ok(false);
    }
    let base = format!(
        "{}/{}",
        manifest.repository.remote, manifest.repository.base_branch
    );
    Ok(git(&["merge-base", "--is-ancestor", target, &base])?
        .status
        .success())
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

fn checkpoint_reference(
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
    println!(
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
        // asserted to be 51 too; change one side and the other must follow.
        assert_eq!(max_flow_nodes(&manifest), 51);
    }

    #[test]
    fn flow_node_bound_covers_lanes_that_fail_at_merge() {
        // A lane that fails at merge spends every success-path node and then
        // its machinery retry, diff, diagnosis, and steering on top. maxNodes
        // counts cumulative rows, so the budget must hold all of them at once:
        // a machinery fault past its retry budget records the retry receipt
        // node and is steered in the same pass.
        const PASS_MAINTENANCE: usize = 3;
        const LANE_SUCCESS_PATH: usize = 7;
        const LANE_FAILURE_PATH: usize = 4;

        for max_parallel in 1..=4 {
            for gate_count in 0..=3 {
                let mut value = manifest_value_for_test(json!([]));
                let object = value.as_object_mut().unwrap();
                object.insert("maxParallel".into(), json!(max_parallel));
                object.insert(
                    "gates".into(),
                    Value::Array(
                        (0..gate_count)
                            .map(|index| {
                                json!({
                                    "kind": "forbidPaths",
                                    "id": format!("no-databases-{index}"),
                                    "forbidPaths": ["*.db"]
                                })
                            })
                            .collect(),
                    ),
                );
                let manifest: CampaignManifest = serde_json::from_value(value).unwrap();

                // No command gates here, so preflight costs nothing.
                let worst_case = PASS_MAINTENANCE
                    + max_parallel * (LANE_SUCCESS_PATH + LANE_FAILURE_PATH + 2 * gate_count);
                assert!(
                    max_flow_nodes(&manifest) as usize >= worst_case,
                    "maxParallel {max_parallel} with {gate_count} gates budgets {} nodes \
                     but a frontier failing at merge needs {worst_case}",
                    max_flow_nodes(&manifest)
                );
            }
        }
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
            last_observation: None,
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
    }
}
