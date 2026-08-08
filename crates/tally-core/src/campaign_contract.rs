//! Canonical contract for forge-native campaign admission and dispatch.
//!
//! The arm CLI is the authority for parsing, defaulting, validating, and
//! normalizing a campaign manifest.  The resulting graph is carried across
//! the flow boundary verbatim; consumers must not rebuild it from forge data.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
#[serde(tag = "kind")]
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
    #[serde(default)]
    pub conflict_domains: Vec<String>,
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(default)]
    pub runtime_max_sec: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CampaignSteward {
    pub adapter: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_steward_final_message_pattern")]
    pub final_message_pattern: Option<String>,
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
                    .map_err(|error| {
                        invalid(format!(
                            "campaign task {} conflictDomains: {error}",
                            task.id
                        ))
                    })?;
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

pub fn validate_agent(agent: &CampaignAgent) -> Result<(), CampaignContractError> {
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

pub fn validate_conflict_domains(
    domains: &[String],
    required: bool,
) -> Result<(), CampaignContractError> {
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

fn default_steward_final_message_pattern() -> Option<String> {
    Some(DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN.to_owned())
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
    fn an_explicit_minimal_steward_is_fully_canonical() {
        let steward: CampaignSteward = serde_json::from_value(json!({
            "adapter": "narrator",
            "argv": ["narrate"]
        }))
        .unwrap();
        assert_eq!(steward.env, BTreeMap::new());
        assert_eq!(
            steward.final_message_pattern.as_deref(),
            Some(DEFAULT_STEWARD_FINAL_MESSAGE_PATTERN)
        );
        assert_eq!(
            steward.runtime_max_sec,
            Some(DEFAULT_STEWARD_RUNTIME_MAX_SEC)
        );
    }
}
