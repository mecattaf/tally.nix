//! Canonical contract for forge-native campaign admission and dispatch.
//!
//! The arm CLI is the authority for parsing, defaulting, validating, and
//! normalizing a campaign manifest.  The resulting graph is carried across
//! the flow boundary verbatim; consumers must not rebuild it from forge data.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const CAMPAIGN_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_MAX_TASKS: usize = 64;
pub const DEFAULT_DRIVER_RUNTIME_MAX_SEC: u64 = 900;
pub const DEFAULT_AGENT_RUNTIME_MAX_SEC: u64 = 14_400;
pub const DEFAULT_RUNNER_POOL: &str = "campaign";
pub const DEFAULT_AGENT_PRIORITY: &str = "low";
pub const DEFAULT_AGENT_APPROVAL_POLICY: &str = "never";
pub const DEFAULT_AGENT_SANDBOX_POLICY: &str = "danger-full-access";
pub const DEFAULT_AGENT_DIAGNOSIS_SANDBOX_POLICY: &str = "read-only";
pub const DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN: &str = "^TALLY_FINAL_MESSAGE=(.*)$";
pub const DEFAULT_STEWARD_RUNTIME_MAX_SEC: u64 = 120;
pub const MAX_AGENT_MODEL_CHARS: usize = 128;
pub const MAX_STEWARD_ENV_ENTRIES: usize = 64;
pub const MAX_STEWARD_ENV_VALUE_CHARS: usize = 4096;
pub const MAX_STEWARD_PATTERN_CHARS: usize = 1024;
pub const BRIEF_SENTINEL: &str = "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set.";

#[derive(Debug, Error)]
#[error("{0}")]
pub struct CampaignContractError(String);

fn invalid(message: impl Into<String>) -> CampaignContractError {
    CampaignContractError(message.into())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignRepository {
    pub checkout: PathBuf,
    #[serde(default = "default_base_branch")]
    pub base_branch: String,
    #[serde(default = "default_remote")]
    pub remote: String,
    #[serde(default = "default_forge")]
    pub forge: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignAgent {
    #[serde(default = "default_agent_adapter")]
    pub adapter: String,
    #[serde(default = "default_agent_argv")]
    pub argv: Vec<String>,
    #[serde(default = "default_agent_priority")]
    pub priority: String,
    #[serde(default = "default_agent_runtime_max_sec")]
    pub runtime_max_sec: Option<u64>,
    #[serde(default = "default_agent_approval_policy")]
    pub approval_policy: Option<String>,
    #[serde(default = "default_agent_sandbox_policy")]
    pub sandbox_policy: Option<String>,
    /// Named adapter sandbox policy for diagnosis nodes.
    #[serde(default = "default_agent_diagnosis_sandbox_policy")]
    pub diagnosis_sandbox_policy: Option<String>,
    /// Absent leaves model selection to the adapter.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum CampaignGate {
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
    pub fn id(&self) -> &str {
        match self {
            Self::Command { id, .. } | Self::ForbidPaths { id, .. } => id,
        }
    }

    pub const fn is_command(&self) -> bool {
        matches!(self, Self::Command { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignTaskReference {
    pub id: String,
    pub kind: String,
    pub issue: u64,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_present_conflict_domains"
    )]
    pub conflict_domains: Option<Vec<String>>,
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub runtime_max_sec: Option<u64>,
}

fn deserialize_present_conflict_domains<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<Vec<String>>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| serde::de::Error::custom("conflictDomains must be an array when present"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignSteward {
    pub adapter: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_steward_final_message_pattern")]
    pub final_message_pattern: String,
    #[serde(default = "default_steward_runtime_max_sec")]
    pub runtime_max_sec: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignManifest {
    pub schema_version: u32,
    pub name: String,
    pub repository: CampaignRepository,
    #[serde(default = "default_max_tasks")]
    pub max_tasks: usize,
    #[serde(default = "default_max_parallel")]
    pub max_parallel: usize,
    #[serde(default = "default_driver_runtime_max_sec")]
    pub driver_runtime_max_sec: u64,
    #[serde(default = "default_campaign_runtime_max_sec")]
    pub runtime_max_sec: Option<u64>,
    #[serde(default = "default_runner_pool")]
    pub pool: String,
    #[serde(default = "default_merge_method")]
    pub merge_method: String,
    #[serde(default = "default_git_ai_binding")]
    pub git_ai_binding: String,
    #[serde(default = "default_git_ai_await_sec")]
    pub git_ai_await_sec: u64,
    pub agent: CampaignAgent,
    #[serde(default)]
    pub steward: Option<CampaignSteward>,
    pub gates: Vec<CampaignGate>,
    pub tasks: Vec<CampaignTaskReference>,
}

/// Immutable issue content included in the executable campaign graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CanonicalCampaignTaskV1 {
    pub number: u64,
    pub title: String,
    pub body: String,
}

/// The exact graph admitted by Rust and consumed by the packaged driver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CanonicalCampaignGraphV1 {
    pub manifest: CampaignManifest,
    pub tasks: Vec<CanonicalCampaignTaskV1>,
    pub executable_digest: String,
}

impl CanonicalCampaignGraphV1 {
    pub fn new(
        manifest: CampaignManifest,
        tasks: Vec<CanonicalCampaignTaskV1>,
    ) -> Result<Self, CampaignContractError> {
        let executable_digest = executable_digest(&manifest, &tasks)?;
        Ok(Self {
            manifest,
            tasks,
            executable_digest,
        })
    }

    /// Compact, recursively key-sorted UTF-8 JSON for the complete envelope.
    pub fn canonical_json(&self) -> Result<String, CampaignContractError> {
        canonical_json(self)
    }
}

/// Parse a raw manifest, apply defaults, resolve checkout identity, and
/// validate the resulting canonical manifest exactly once.
pub fn admit_manifest_json(json: &str) -> Result<CampaignManifest, CampaignContractError> {
    let manifest = serde_json::from_str(json)
        .map_err(|error| invalid(format!("campaign manifest is invalid: {error}")))?;
    normalize_and_validate_manifest(manifest)
}

/// Value-shaped entry point used by project rendering before it mutates forge
/// state. It has the same admission semantics as the issue-body parser.
pub fn admit_manifest_value(value: Value) -> Result<CampaignManifest, CampaignContractError> {
    let manifest = serde_json::from_value(value)
        .map_err(|error| invalid(format!("campaign manifest is invalid: {error}")))?;
    normalize_and_validate_manifest(manifest)
}

fn normalize_and_validate_manifest(
    mut manifest: CampaignManifest,
) -> Result<CampaignManifest, CampaignContractError> {
    if !manifest.repository.checkout.is_absolute() {
        return Err(invalid("campaign repository.checkout must be absolute"));
    }
    let original = manifest.repository.checkout.clone();
    let checkout = fs::canonicalize(&original).map_err(|error| {
        invalid(format!(
            "cannot canonicalize campaign repository checkout {}: {error}",
            original.display()
        ))
    })?;
    if !checkout.is_dir() || !is_git_worktree(&checkout)? {
        return Err(invalid(format!(
            "campaign repository checkout is not a Git worktree: {}",
            checkout.display()
        )));
    }
    manifest.repository.checkout = checkout;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn is_git_worktree(checkout: &Path) -> Result<bool, CampaignContractError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|error| {
            invalid(format!(
                "cannot execute git while validating campaign checkout: {error}"
            ))
        })?;
    Ok(output.status.success() && output.stdout == b"true\n")
}

pub fn validate_manifest(manifest: &CampaignManifest) -> Result<(), CampaignContractError> {
    if manifest.schema_version != CAMPAIGN_SCHEMA_VERSION {
        return Err(invalid("campaign manifest schemaVersion must equal 1"));
    }
    if !safe_component(&manifest.name) {
        return Err(invalid("campaign name is not a safe component"));
    }
    if !manifest.repository.checkout.is_absolute() {
        return Err(invalid("campaign repository.checkout must be absolute"));
    }
    if !non_empty_without_controls(&manifest.repository.base_branch) {
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
        validate_argv(&steward.argv, "campaign steward argv")?;
        if steward.runtime_max_sec == Some(0) {
            return Err(invalid("campaign steward runtimeMaxSec must be positive"));
        }
        if steward.env.len() > MAX_STEWARD_ENV_ENTRIES {
            return Err(invalid(format!(
                "campaign steward env must contain at most {MAX_STEWARD_ENV_ENTRIES} entries"
            )));
        }
        for (name, value) in &steward.env {
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
            if !non_empty_bounded_without_controls(value, MAX_STEWARD_ENV_VALUE_CHARS) {
                return Err(invalid(format!(
                    "campaign steward env value for {name:?} must be non-empty, contain no control characters, and contain at most {MAX_STEWARD_ENV_VALUE_CHARS} characters"
                )));
            }
        }
        validate_steward_pattern(&steward.final_message_pattern)?;
    }
    validate_gates(&manifest.gates)?;
    if manifest.tasks.is_empty() || manifest.tasks.len() > manifest.max_tasks {
        return Err(invalid(format!(
            "campaign contains {} tasks, but manifest.maxTasks permits 1..={}",
            manifest.tasks.len(),
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
                validate_conflict_domains(
                    task.conflict_domains.as_deref(),
                    manifest.max_parallel > 1,
                )
                .map_err(|error| {
                    invalid(format!(
                        "campaign task {} conflictDomains: {error}",
                        task.id
                    ))
                })?;
            }
            "checkpoint" => {
                if task.conflict_domains.is_some() {
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

pub fn validate_agent(agent: &CampaignAgent) -> Result<(), CampaignContractError> {
    if !non_empty_without_controls(&agent.adapter) {
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
        || agent
            .approval_policy
            .as_deref()
            .is_some_and(|value| !non_empty_without_controls(value))
        || agent
            .sandbox_policy
            .as_deref()
            .is_some_and(|value| !non_empty_without_controls(value))
        || agent
            .diagnosis_sandbox_policy
            .as_deref()
            .is_some_and(|value| !non_empty_without_controls(value))
        || agent
            .model
            .as_deref()
            .is_some_and(|value| !non_empty_bounded_without_controls(value, MAX_AGENT_MODEL_CHARS))
    {
        return Err(invalid(
            "campaign agent limits, policy names, and model must be non-empty and bounded",
        ));
    }
    Ok(())
}

pub fn validate_gates(gates: &[CampaignGate]) -> Result<(), CampaignContractError> {
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
                        || pattern.chars().count() > 1024
                        || pattern.starts_with('/')
                        || pattern.ends_with('/')
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

pub fn validate_argv(argv: &[String], context: &str) -> Result<(), CampaignContractError> {
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

/// Validate the deliberately small regular-expression language shared with
/// Python's `re` runtime. Rust owns admission; Python only compiles the
/// admitted value defensively before execution.
pub fn validate_steward_pattern(pattern: &str) -> Result<(), CampaignContractError> {
    if !non_empty_bounded_without_controls(pattern, MAX_STEWARD_PATTERN_CHARS) {
        return Err(invalid(format!(
            "campaign steward finalMessagePattern must be non-empty, contain no control characters, and contain at most {MAX_STEWARD_PATTERN_CHARS} characters"
        )));
    }

    let characters = pattern.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut in_class = false;
    while index < characters.len() {
        let character = characters[index];
        if character == '\\' {
            index += 1;
            let Some(escaped) = characters.get(index).copied() else {
                return Err(invalid(
                    "campaign steward finalMessagePattern ends with an incomplete escape",
                ));
            };
            if escaped.is_ascii_digit() || matches!(escaped, 'k' | 'g') {
                return Err(invalid(
                    "campaign steward finalMessagePattern must not contain backreferences",
                ));
            }
            if escaped == 'x' {
                let hex = characters.get(index + 1..index + 3).unwrap_or_default();
                if hex.len() != 2 || !hex.iter().all(|candidate| candidate.is_ascii_hexdigit()) {
                    return Err(invalid(
                        "campaign steward finalMessagePattern contains an invalid hexadecimal escape",
                    ));
                }
                index += 2;
            } else if !matches!(
                escaped,
                '\\' | '.'
                    | '^'
                    | '$'
                    | '|'
                    | '?'
                    | '*'
                    | '+'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '-'
                    | '/'
                    | 'd'
                    | 'D'
                    | 's'
                    | 'S'
                    | 'w'
                    | 'W'
                    | 'n'
                    | 'r'
                    | 't'
                    | 'f'
            ) {
                return Err(invalid(format!(
                    "campaign steward finalMessagePattern contains non-portable escape \\{escaped}"
                )));
            }
        } else if character == '[' {
            if in_class {
                return Err(invalid(
                    "campaign steward finalMessagePattern contains a non-portable nested character class",
                ));
            }
            in_class = true;
        } else if character == ']' {
            in_class = false;
        } else if in_class
            && index + 1 < characters.len()
            && matches!(
                (character, characters[index + 1]),
                ('&', '&') | ('-', '-') | ('~', '~') | ('|', '|')
            )
        {
            return Err(invalid(
                "campaign steward finalMessagePattern contains a non-portable character-class operation",
            ));
        } else if !in_class
            && character == '('
            && characters.get(index + 1) == Some(&'?')
            && characters.get(index + 2) != Some(&':')
        {
            return Err(invalid(
                "campaign steward finalMessagePattern must not contain look-around, named or conditional groups, or inline flags",
            ));
        }
        index += 1;
    }

    let compiled = Regex::new(pattern).map_err(|error| {
        invalid(format!(
            "campaign steward finalMessagePattern is not valid in the portable regex subset: {error}"
        ))
    })?;
    let captures = compiled.captures_len().saturating_sub(1);
    if captures != 1 {
        return Err(invalid(format!(
            "campaign steward finalMessagePattern must contain exactly one capture group, found {captures}"
        )));
    }
    Ok(())
}

pub fn validate_conflict_domains(
    domains: Option<&[String]>,
    required: bool,
) -> Result<(), CampaignContractError> {
    let Some(domains) = domains else {
        if required {
            return Err(invalid(
                "must be non-empty when campaign maxParallel is greater than one",
            ));
        }
        return Ok(());
    };
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

pub fn executable_digest(
    manifest: &CampaignManifest,
    tasks: &[CanonicalCampaignTaskV1],
) -> Result<String, CampaignContractError> {
    #[derive(Serialize)]
    struct ExecutableGraph<'a> {
        manifest: &'a CampaignManifest,
        tasks: &'a [CanonicalCampaignTaskV1],
    }
    let bytes = canonical_json(ExecutableGraph { manifest, tasks })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes.as_bytes())))
}

/// Revision identity for one task's durable completion proof.
///
/// The campaign's full executable digest remains the admission boundary, but
/// it is intentionally too broad for completion: adding an unrelated task or
/// editing another brief must not invalidate every merged pull request. This
/// contract includes the task itself plus the global execution policy that can
/// change whether its proof is valid, and excludes scheduler/capacity fields.
pub fn task_completion_revision(
    manifest: &CampaignManifest,
    reference: &CampaignTaskReference,
    content: &CanonicalCampaignTaskV1,
) -> Result<String, CampaignContractError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct CompletionPolicy<'a> {
        contract_version: u32,
        campaign: &'a str,
        repository: &'a CampaignRepository,
        merge_method: &'a str,
        git_ai_binding: &'a str,
        git_ai_await_sec: u64,
        agent: &'a CampaignAgent,
        steward: &'a Option<CampaignSteward>,
        gates: &'a [CampaignGate],
        task: &'a CampaignTaskReference,
        content: &'a CanonicalCampaignTaskV1,
    }

    let bytes = canonical_json(CompletionPolicy {
        contract_version: 1,
        campaign: &manifest.name,
        repository: &manifest.repository,
        merge_method: &manifest.merge_method,
        git_ai_binding: &manifest.git_ai_binding,
        git_ai_await_sec: manifest.git_ai_await_sec,
        agent: &manifest.agent,
        steward: &manifest.steward,
        gates: &manifest.gates,
        task: reference,
        content,
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes.as_bytes())))
}

/// Compact recursively key-sorted JSON. This is deliberately independent of
/// map insertion order so every consumer can reproduce the bytes.
pub fn canonical_json(value: impl Serialize) -> Result<String, CampaignContractError> {
    fn write(value: &Value, output: &mut String) -> Result<(), serde_json::Error> {
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

    let value = serde_json::to_value(value)
        .map_err(|error| invalid(format!("cannot serialize canonical campaign JSON: {error}")))?;
    let mut output = String::new();
    write(&value, &mut output)
        .map_err(|error| invalid(format!("cannot serialize canonical campaign JSON: {error}")))?;
    Ok(output)
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

fn non_empty_without_controls(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_control)
}

fn non_empty_bounded_without_controls(value: &str, maximum: usize) -> bool {
    non_empty_without_controls(value) && value.chars().count() <= maximum
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

fn default_steward_final_message_pattern() -> String {
    DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN.to_owned()
}

const fn default_steward_runtime_max_sec() -> Option<u64> {
    Some(DEFAULT_STEWARD_RUNTIME_MAX_SEC)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_is_recursive_and_stable() {
        let value = json!({"z": [1, "é"], "a": {"b": true, "a": null}});
        assert_eq!(
            canonical_json(&value).unwrap(),
            r#"{"a":{"a":null,"b":true},"z":[1,"é"]}"#
        );
        assert_eq!(
            format!(
                "sha256:{:x}",
                Sha256::digest(canonical_json(&value).unwrap().as_bytes())
            ),
            "sha256:356741b14061aca3cb3e9abc01fe332af042dfcd59d81c56ee9fb57832dc6429"
        );
    }

    #[test]
    fn task_completion_revision_ignores_unrelated_graph_edits() {
        let manifest: CampaignManifest = serde_json::from_value(json!({
            "schemaVersion": 1,
            "name": "fixture",
            "repository": {"checkout": "/srv/fixture", "forge": "github"},
            "agent": {},
            "gates": [],
            "tasks": [{
                "id": "build",
                "kind": "implementation",
                "issue": 8,
                "dependencies": [],
                "conflictDomains": ["src/build"]
            }]
        }))
        .unwrap();
        let content = CanonicalCampaignTaskV1 {
            number: 8,
            title: "Build the feature".to_owned(),
            body: "Implement the admitted feature.".to_owned(),
        };
        let revision = task_completion_revision(&manifest, &manifest.tasks[0], &content).unwrap();

        let mut unrelated = manifest.clone();
        unrelated.max_tasks = 2;
        unrelated.max_parallel = 2;
        unrelated.pool = "larger-campaign-pool".to_owned();
        unrelated.tasks.push(
            serde_json::from_value(json!({
                "id": "document",
                "kind": "implementation",
                "issue": 9,
                "dependencies": [],
                "conflictDomains": ["doc"]
            }))
            .unwrap(),
        );
        assert_eq!(
            task_completion_revision(&unrelated, &unrelated.tasks[0], &content).unwrap(),
            revision,
            "scheduler changes and a new sibling task must not invalidate a merged task"
        );

        let mut own_reference_changed = unrelated.clone();
        own_reference_changed.tasks[0]
            .dependencies
            .push("document".to_owned());
        assert_ne!(
            task_completion_revision(
                &own_reference_changed,
                &own_reference_changed.tasks[0],
                &content,
            )
            .unwrap(),
            revision,
            "the completed task's own dependency contract remains revision-bearing"
        );

        let mut own_content_changed = content.clone();
        own_content_changed.body = "Implement the edited feature.".to_owned();
        assert_ne!(
            task_completion_revision(&manifest, &manifest.tasks[0], &own_content_changed).unwrap(),
            revision,
            "the completed task's admitted issue content remains revision-bearing"
        );

        let mut global_policy_changed = manifest.clone();
        global_policy_changed.gates.push(CampaignGate::Command {
            id: "tests".to_owned(),
            preflight_argv: vec!["true".to_owned()],
            argv: vec!["true".to_owned()],
            runtime_max_sec: 60,
        });
        assert_ne!(
            task_completion_revision(
                &global_policy_changed,
                &global_policy_changed.tasks[0],
                &content,
            )
            .unwrap(),
            revision,
            "global execution policy changes must invalidate prior task proof"
        );
    }

    #[test]
    fn an_explicit_minimal_steward_is_fully_canonical() {
        let steward: CampaignSteward = serde_json::from_value(json!({
            "adapter": "narrator",
            "argv": ["narrate"]
        }))
        .unwrap();
        assert_eq!(steward.env, BTreeMap::new());
        assert_eq!(
            steward.final_message_pattern,
            DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN
        );
        assert_eq!(
            steward.runtime_max_sec,
            Some(DEFAULT_STEWARD_RUNTIME_MAX_SEC)
        );
    }

    #[test]
    fn task_reference_json_preserves_all_three_conflict_domain_states() {
        let omitted: CampaignTaskReference = serde_json::from_value(json!({
            "id": "serial",
            "kind": "implementation",
            "issue": 1,
            "dependencies": []
        }))
        .unwrap();
        assert_eq!(omitted.conflict_domains, None);
        assert!(
            serde_json::to_value(&omitted).unwrap()["conflictDomains"].is_null(),
            "indexing a missing object key yields null; the key itself is checked below"
        );
        assert!(!serde_json::to_value(&omitted)
            .unwrap()
            .as_object()
            .unwrap()
            .contains_key("conflictDomains"));

        let empty: CampaignTaskReference = serde_json::from_value(json!({
            "id": "empty",
            "kind": "implementation",
            "issue": 2,
            "conflictDomains": []
        }))
        .unwrap();
        assert_eq!(empty.conflict_domains, Some(Vec::new()));
        assert_eq!(
            serde_json::to_value(&empty).unwrap()["conflictDomains"],
            json!([])
        );

        let declared: CampaignTaskReference = serde_json::from_value(json!({
            "id": "declared",
            "kind": "implementation",
            "issue": 3,
            "conflictDomains": ["src"]
        }))
        .unwrap();
        assert_eq!(declared.conflict_domains, Some(vec!["src".to_owned()]));
        assert!(serde_json::from_value::<CampaignTaskReference>(json!({
            "id": "null",
            "kind": "implementation",
            "issue": 4,
            "conflictDomains": null
        }))
        .is_err());
    }

    #[test]
    fn parallelism_requires_a_present_nonempty_conflict_domain() {
        validate_conflict_domains(None, false).unwrap();
        validate_conflict_domains(Some(&[]), false).unwrap();
        assert!(validate_conflict_domains(None, true).is_err());
        assert!(validate_conflict_domains(Some(&[]), true).is_err());
        validate_conflict_domains(Some(&["src".to_owned()]), true).unwrap();
    }

    #[test]
    fn steward_pattern_subset_is_portable_and_single_capture() {
        for accepted in [
            DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN,
            r"^(?:résultat: )[A-Z\d_-]*(.*)$",
            r"^value=(\x41.*)$",
        ] {
            validate_steward_pattern(accepted).unwrap();
        }
        for rejected in [
            "(",
            "^no capture$",
            "^(one)(two)$",
            r"^(a)\1$",
            r"^(?=(.*))$",
            r"^(?P<answer>.*)$",
            r"(?i)^(.*)$",
            r"^([a&&b]*)$",
        ] {
            assert!(
                validate_steward_pattern(rejected).is_err(),
                "pattern should be rejected: {rejected}"
            );
        }
    }
}
