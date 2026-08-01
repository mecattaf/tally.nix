use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json_path::JsonPath;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::executor::ExecutionPaths;

const MAX_CAPTURE_BYTES: u64 = 16 * 1024 * 1024;

pub fn provisions_gate_manifest(adapter: &str) -> bool {
    matches!(adapter, "claude-code" | "codex")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrapeStream {
    #[default]
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScrapeMode {
    #[default]
    Regex,
    JsonPath,
    /// Evaluate one JSONPath over the complete JSON-lines stream and retain
    /// its last non-null match. This lets presets predicate on event shape
    /// without accidentally promoting similarly named fields from other
    /// events.
    JsonPathLast,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterHardening {
    Production,
    Strict,
    Workspace,
    #[default]
    None,
}

impl AdapterHardening {
    pub const fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceFraming {
    #[default]
    JsonLines,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdapterTrace {
    #[serde(default)]
    pub stream: ScrapeStream,
    #[serde(default)]
    pub framing: TraceFraming,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ScrapeCapture {
    #[serde(default)]
    pub stream: ScrapeStream,
    #[serde(default)]
    pub mode: ScrapeMode,
    pub pattern: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdapterValueOverride {
    pub argv: Vec<String>,
    #[serde(default)]
    pub allowed_values: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdapterLaunchConfig {
    #[serde(default)]
    pub allow_pre_prompt_argv: bool,
    #[serde(default)]
    pub cwd_argv: Option<Vec<String>>,
    #[serde(default)]
    pub approval_policies: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub sandbox_policies: BTreeMap<String, Vec<String>>,
    /// Subset of `sandbox_policies` under which the adapter's agent can create
    /// a commit. An adapter that leaves this empty declares nothing, and a
    /// caller with a commit obligation cannot be held to anything. An adapter
    /// that declares it makes "this policy can write but not commit" a
    /// refusable configuration instead of a three-second campaign failure.
    #[serde(default)]
    pub commit_capable_sandbox_policies: BTreeSet<String>,
    #[serde(default)]
    pub model: Option<AdapterValueOverride>,
    #[serde(default)]
    pub effort: Option<AdapterValueOverride>,
}

impl AdapterLaunchConfig {
    /// Whether the adapter says anything at all about which sandbox policies
    /// permit a commit. Silence is not consent and not refusal: a caller with a
    /// commit obligation may only refuse an adapter that has spoken.
    #[must_use]
    pub fn declares_commit_capability(&self) -> bool {
        !self.commit_capable_sandbox_policies.is_empty()
    }

    /// Whether `policy` may be used by a node whose obligation is a commit. An
    /// adapter that declares nothing answers yes to everything; an adapter that
    /// declares a set answers no to the adapter default (`None`) as well,
    /// because an unnamed policy is exactly what shipped a non-committing
    /// sandbox into a campaign.
    #[must_use]
    pub fn permits_commit(&self, policy: Option<&str>) -> bool {
        if !self.declares_commit_capability() {
            return true;
        }
        policy.is_some_and(|policy| self.commit_capable_sandbox_policies.contains(policy))
    }

    /// Comma-separated commit-capable policy names, for refusal messages.
    #[must_use]
    pub fn commit_capable_names(&self) -> String {
        self.commit_capable_sandbox_policies
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdapterJobOptions {
    #[serde(default)]
    pub pre_prompt_argv: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl AdapterJobOptions {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AdapterConfig {
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub resume: Option<Vec<String>>,
    #[serde(default)]
    pub scrape: BTreeMap<String, ScrapeCapture>,
    #[serde(default)]
    pub trace: Option<AdapterTrace>,
    #[serde(default)]
    pub yield_hook: Option<Vec<String>>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub launch: AdapterLaunchConfig,
    #[serde(default, skip_serializing_if = "AdapterHardening::is_none")]
    pub hardening: AdapterHardening,
    #[serde(default)]
    pub extra_writable_paths: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_bundle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_revision: Option<String>,
    #[serde(default)]
    pub extra_config: BTreeMap<String, Value>,
}

impl AdapterConfig {
    /// Resolve the replay-stable skill or agent-definition revision carried by
    /// this adapter configuration.
    #[must_use]
    pub fn resolved_skill_revision(&self) -> Option<String> {
        self.skill_bundle
            .as_deref()
            .map(|bundle| format!("sha256:{:x}", Sha256::digest(bundle.as_bytes())))
            .or_else(|| self.skill_revision.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AdapterInvocation {
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub hardening: AdapterHardening,
    pub extra_writable_paths: Vec<PathBuf>,
    /// Direct checkpoint probe installed by the harness integration. Tally
    /// deliberately does not run it on a timer: the harness owns its safe
    /// checkpoints, and the no-argument preset probe resolves TALLY_JOB_ID.
    pub yield_hook: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ScrapeResult {
    pub captures: BTreeMap<String, Value>,
}

impl ScrapeResult {
    pub fn string(&self, name: &str) -> Result<Option<&str>, AdapterError> {
        match self.captures.get(name) {
            None => Ok(None),
            Some(Value::String(value)) => Ok(Some(value)),
            Some(_) => Err(AdapterError::ReservedCaptureType(name.to_owned())),
        }
    }

    pub fn session_ref(&self) -> Result<Option<&str>, AdapterError> {
        self.string("sessionRef")
    }

    pub fn model(&self) -> Result<Option<&str>, AdapterError> {
        self.string("model")
    }

    pub fn final_message(&self) -> Result<Option<&str>, AdapterError> {
        self.string("finalMessage")
    }
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("unknown adapter {0:?}")]
    UnknownAdapter(String),
    #[error("invalid adapter {adapter:?}: {detail}")]
    InvalidConfig { adapter: String, detail: String },
    #[error("adapter {0:?} has no resume template")]
    NoResume(String),
    #[error("resume capture {capture:?} is absent for adapter {adapter:?}")]
    MissingCapture { adapter: String, capture: String },
    #[error("reserved capture {0:?} must be a JSON string")]
    ReservedCaptureType(String),
    #[error("capture path {path} cannot be read: {source}")]
    CaptureRead { path: String, source: io::Error },
    #[error("capture path {0} is not a regular file")]
    CaptureNotRegular(String),
    #[error("capture path {path} exceeds the {limit}-byte scrape bound")]
    CaptureTooLarge { path: String, limit: u64 },
    #[error("capture path {0} is not UTF-8")]
    CaptureNotUtf8(String),
    #[error("capture {capture:?} could not be scraped: {detail}")]
    Scrape { capture: String, detail: String },
}

pub struct AdapterEngine<'a> {
    adapters: &'a BTreeMap<String, AdapterConfig>,
}

impl<'a> AdapterEngine<'a> {
    pub const fn new(adapters: &'a BTreeMap<String, AdapterConfig>) -> Self {
        Self { adapters }
    }

    pub fn validate_all(&self) -> Result<(), AdapterError> {
        for (name, adapter) in self.adapters {
            validate_adapter_name(name)?;
            validate_adapter(name, adapter)?;
        }
        Ok(())
    }

    pub fn adapter(&self, name: &str) -> Result<&AdapterConfig, AdapterError> {
        self.adapters
            .get(name)
            .ok_or_else(|| AdapterError::UnknownAdapter(name.to_owned()))
    }

    pub fn launch(
        &self,
        name: &str,
        workload_argv: &[String],
    ) -> Result<AdapterInvocation, AdapterError> {
        self.launch_with_options(name, workload_argv, &AdapterJobOptions::default(), None)
    }

    pub fn launch_with_options(
        &self,
        name: &str,
        workload_argv: &[String],
        options: &AdapterJobOptions,
        cwd: Option<&Path>,
    ) -> Result<AdapterInvocation, AdapterError> {
        let adapter = self.adapter(name)?;
        let prefix = render_launch_prefix(name, adapter, &adapter.argv, options, cwd)?;
        invocation(name, adapter, options, compose_argv(&prefix, workload_argv))
    }

    pub fn resume(
        &self,
        name: &str,
        workload_argv: &[String],
        captures: &ScrapeResult,
    ) -> Result<AdapterInvocation, AdapterError> {
        self.resume_with_options(
            name,
            workload_argv,
            captures,
            &AdapterJobOptions::default(),
            None,
        )
    }

    pub fn resume_with_options(
        &self,
        name: &str,
        workload_argv: &[String],
        captures: &ScrapeResult,
        options: &AdapterJobOptions,
        cwd: Option<&Path>,
    ) -> Result<AdapterInvocation, AdapterError> {
        let adapter = self.adapter(name)?;
        let template = adapter
            .resume
            .as_deref()
            .ok_or_else(|| AdapterError::NoResume(name.to_owned()))?;
        validate_requested_value(
            name,
            "model",
            options.model.as_deref(),
            adapter.launch.model.as_ref(),
        )?;
        validate_requested_value(
            name,
            "effort",
            options.effort.as_deref(),
            adapter.launch.effort.as_ref(),
        )?;
        let mut resume_captures = captures.captures.clone();
        if let Some(model) = &options.model {
            resume_captures.insert("model".to_owned(), Value::String(model.clone()));
        }
        let template_has_cwd = template.iter().any(|argument| argument.contains("%<cwd>%"));
        if let Some(cwd) = cwd.filter(|_| template_has_cwd) {
            let cwd = cwd.to_str().ok_or_else(|| AdapterError::InvalidConfig {
                adapter: name.to_owned(),
                detail: "cwd must be valid UTF-8 for adapter argv rendering".to_owned(),
            })?;
            resume_captures.insert("cwd".to_owned(), Value::String(cwd.to_owned()));
        }
        let rendered = template
            .iter()
            .map(|argument| render_argument(name, argument, &resume_captures))
            .collect::<Result<Vec<_>, _>>()?;
        let mut inserted_options = options.clone();
        if template
            .iter()
            .any(|argument| argument.contains("%<model>%"))
        {
            inserted_options.model = None;
        }
        if template
            .iter()
            .any(|argument| argument.contains("%<effort>%"))
        {
            inserted_options.effort = None;
        }
        let prefix = render_launch_prefix(
            name,
            adapter,
            &rendered,
            &inserted_options,
            (!template_has_cwd).then_some(cwd).flatten(),
        )?;
        invocation(name, adapter, options, compose_argv(&prefix, workload_argv))
    }

    pub fn scrape_text(
        &self,
        name: &str,
        stdout: &str,
        stderr: &str,
    ) -> Result<ScrapeResult, AdapterError> {
        let adapter = self.adapter(name)?;
        let mut captures = BTreeMap::new();
        for (capture_name, capture) in &adapter.scrape {
            let input = match capture.stream {
                ScrapeStream::Stdout => stdout,
                ScrapeStream::Stderr => stderr,
            };
            let value = match capture.mode {
                ScrapeMode::Regex => scrape_regex(&capture.pattern, input),
                ScrapeMode::JsonPath => scrape_json_path(&capture.pattern, input),
                ScrapeMode::JsonPathLast => scrape_json_path_last(&capture.pattern, input),
            }
            .map_err(|detail| AdapterError::Scrape {
                capture: capture_name.clone(),
                detail,
            })?;
            if let Some(value) = value {
                captures.insert(capture_name.clone(), value);
            }
        }
        let result = ScrapeResult { captures };
        result.session_ref()?;
        result.model()?;
        result.final_message()?;
        Ok(result)
    }

    pub fn scrape_paths(
        &self,
        name: &str,
        paths: &ExecutionPaths,
    ) -> Result<ScrapeResult, AdapterError> {
        let adapter = self.adapter(name)?;
        let needs_stdout = adapter
            .scrape
            .values()
            .any(|capture| capture.stream == ScrapeStream::Stdout);
        let needs_stderr = adapter
            .scrape
            .values()
            .any(|capture| capture.stream == ScrapeStream::Stderr);
        let stdout = if needs_stdout {
            read_capture(&paths.stdout)?
        } else {
            String::new()
        };
        let stderr = if needs_stderr {
            read_capture(&paths.stderr)?
        } else {
            String::new()
        };
        self.scrape_text(name, &stdout, &stderr)
    }
}

fn invocation(
    name: &str,
    adapter: &AdapterConfig,
    options: &AdapterJobOptions,
    argv: Vec<String>,
) -> Result<AdapterInvocation, AdapterError> {
    if argv.is_empty() || argv[0].is_empty() {
        return Err(AdapterError::InvalidConfig {
            adapter: name.to_owned(),
            detail: "rendered argv must contain a non-empty executable".to_owned(),
        });
    }
    if argv.iter().any(|argument| argument.contains('\0')) {
        return Err(AdapterError::InvalidConfig {
            adapter: name.to_owned(),
            detail: "rendered argv must not contain NUL bytes".to_owned(),
        });
    }
    let mut env = adapter.env.clone();
    for (environment, value) in &options.environment {
        if !valid_environment_name(environment)
            || environment.starts_with("TALLY_")
            || environment == "CREDENTIALS_DIRECTORY"
        {
            return Err(AdapterError::InvalidConfig {
                adapter: name.to_owned(),
                detail: format!("job environment name {environment:?} is invalid or reserved"),
            });
        }
        if value.contains('\0') {
            return Err(AdapterError::InvalidConfig {
                adapter: name.to_owned(),
                detail: format!("job environment {environment:?} contains a NUL byte"),
            });
        }
        env.insert(environment.clone(), value.clone());
    }
    Ok(AdapterInvocation {
        argv,
        env,
        hardening: adapter.hardening,
        extra_writable_paths: adapter.extra_writable_paths.clone(),
        yield_hook: adapter.yield_hook.clone(),
    })
}

fn compose_argv(prefix: &[String], workload: &[String]) -> Vec<String> {
    prefix.iter().chain(workload).cloned().collect()
}

fn render_launch_prefix(
    name: &str,
    adapter: &AdapterConfig,
    base: &[String],
    options: &AdapterJobOptions,
    cwd: Option<&Path>,
) -> Result<Vec<String>, AdapterError> {
    if !options.pre_prompt_argv.is_empty() && !adapter.launch.allow_pre_prompt_argv {
        return invalid_config(
            name,
            "job prePromptArgv is not authorized by this adapter".to_owned(),
        );
    }
    if options
        .pre_prompt_argv
        .iter()
        .any(|argument| argument.contains('\0'))
    {
        return invalid_config(
            name,
            "job prePromptArgv must contain no NUL bytes".to_owned(),
        );
    }
    let mut inserted = options.pre_prompt_argv.clone();
    inserted.extend(render_policy(
        name,
        "approvalPolicy",
        options.approval_policy.as_deref(),
        &adapter.launch.approval_policies,
    )?);
    inserted.extend(render_policy(
        name,
        "sandboxPolicy",
        options.sandbox_policy.as_deref(),
        &adapter.launch.sandbox_policies,
    )?);
    if let (Some(template), Some(cwd)) = (&adapter.launch.cwd_argv, cwd) {
        let cwd = cwd.to_str().ok_or_else(|| AdapterError::InvalidConfig {
            adapter: name.to_owned(),
            detail: "cwd must be valid UTF-8 for adapter argv rendering".to_owned(),
        })?;
        inserted.extend(render_named_template(name, template, "cwd", cwd)?);
    }
    inserted.extend(render_value_override(
        name,
        "model",
        options.model.as_deref(),
        adapter.launch.model.as_ref(),
    )?);
    inserted.extend(render_value_override(
        name,
        "effort",
        options.effort.as_deref(),
        adapter.launch.effort.as_ref(),
    )?);

    if inserted.is_empty() {
        return Ok(base.to_vec());
    }
    let Some((delimiter, prefix)) = base.split_last() else {
        return invalid_config(
            name,
            "pre-prompt options require an adapter prefix ending in '--'".to_owned(),
        );
    };
    if delimiter != "--" {
        return invalid_config(
            name,
            "pre-prompt options require an adapter prefix ending in '--'".to_owned(),
        );
    }
    let mut rendered = prefix.to_vec();
    rendered.extend(inserted);
    rendered.push(delimiter.clone());
    Ok(rendered)
}

fn render_policy(
    adapter: &str,
    field: &str,
    requested: Option<&str>,
    policies: &BTreeMap<String, Vec<String>>,
) -> Result<Vec<String>, AdapterError> {
    let Some(requested) = requested else {
        return Ok(Vec::new());
    };
    policies
        .get(requested)
        .cloned()
        .ok_or_else(|| AdapterError::InvalidConfig {
            adapter: adapter.to_owned(),
            detail: format!("{field} value {requested:?} is not authorized by this adapter"),
        })
}

fn render_value_override(
    adapter: &str,
    field: &str,
    requested: Option<&str>,
    config: Option<&AdapterValueOverride>,
) -> Result<Vec<String>, AdapterError> {
    validate_requested_value(adapter, field, requested, config)?;
    match (requested, config) {
        (Some(value), Some(config)) => render_named_template(adapter, &config.argv, "value", value),
        (None, _) => Ok(Vec::new()),
        (Some(_), None) => unreachable!("requested values were validated above"),
    }
}

fn validate_requested_value(
    adapter: &str,
    field: &str,
    requested: Option<&str>,
    config: Option<&AdapterValueOverride>,
) -> Result<(), AdapterError> {
    let Some(requested) = requested else {
        return Ok(());
    };
    let config = config.ok_or_else(|| AdapterError::InvalidConfig {
        adapter: adapter.to_owned(),
        detail: format!("{field} override is not authorized by this adapter"),
    })?;
    if !config
        .allowed_values
        .iter()
        .any(|allowed| allowed == requested)
    {
        return invalid_config(
            adapter,
            format!("{field} value {requested:?} is not authorized by this adapter"),
        );
    }
    Ok(())
}

fn render_named_template(
    adapter: &str,
    template: &[String],
    name: &str,
    value: &str,
) -> Result<Vec<String>, AdapterError> {
    let token = format!("%<{name}>%");
    template
        .iter()
        .map(|argument| {
            let rendered = argument.replace(&token, value);
            if rendered.contains('\0') {
                return Err(AdapterError::InvalidConfig {
                    adapter: adapter.to_owned(),
                    detail: format!("rendered {name} argv contains a NUL byte"),
                });
            }
            Ok(rendered)
        })
        .collect()
}

fn validate_adapter_name(name: &str) -> Result<(), AdapterError> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(AdapterError::InvalidConfig {
            adapter: name.to_owned(),
            detail: "name must be non-empty and contain no control characters".to_owned(),
        });
    }
    Ok(())
}

fn validate_adapter(name: &str, adapter: &AdapterConfig) -> Result<(), AdapterError> {
    if adapter.skill_bundle.is_some() && adapter.skill_revision.is_some() {
        return invalid_config(
            name,
            "skillBundle and skillRevision are mutually exclusive".to_owned(),
        );
    }
    validate_argv(name, "argv", &adapter.argv, true)?;
    if let Some(resume) = &adapter.resume {
        validate_argv(name, "resume", resume, false)?;
    }
    if let Some(hook) = &adapter.yield_hook {
        validate_argv(name, "yieldHook", hook, false)?;
    }
    for path in &adapter.extra_writable_paths {
        if !path.is_absolute() {
            return invalid_config(
                name,
                format!(
                    "extraWritablePaths entry {} must be absolute",
                    path.display()
                ),
            );
        }
        let Some(path) = path.to_str() else {
            return invalid_config(
                name,
                "extraWritablePaths entries must be valid UTF-8".to_owned(),
            );
        };
        if path.chars().any(char::is_control) || path.contains('%') {
            return invalid_config(
                name,
                format!(
                    "extraWritablePaths entry {path:?} must contain no control or systemd specifier characters"
                ),
            );
        }
    }
    for (env_name, value) in &adapter.env {
        if !valid_environment_name(env_name)
            || env_name.starts_with("TALLY_")
            || env_name == "CREDENTIALS_DIRECTORY"
        {
            return invalid_config(
                name,
                format!("environment name {env_name:?} is invalid or reserved"),
            );
        }
        if value.contains('\0') {
            return invalid_config(
                name,
                format!("environment {env_name:?} contains a NUL byte"),
            );
        }
    }
    validate_launch_config(name, &adapter.launch)?;
    let mut names = BTreeSet::new();
    for (capture_name, capture) in &adapter.scrape {
        if !valid_capture_name(capture_name) {
            return invalid_config(name, format!("invalid capture name {capture_name:?}"));
        }
        names.insert(capture_name.as_str());
        if capture.pattern.is_empty() || capture.pattern.contains('\0') {
            return invalid_config(
                name,
                format!("capture {capture_name:?} has an empty or NUL pattern"),
            );
        }
        match capture.mode {
            ScrapeMode::Regex => {
                compile_regex(&capture.pattern).map_err(|error| AdapterError::InvalidConfig {
                    adapter: name.to_owned(),
                    detail: format!("capture {capture_name:?} has invalid regex: {error}"),
                })?;
            }
            ScrapeMode::JsonPath | ScrapeMode::JsonPathLast => {
                parse_json_path(&capture.pattern).map_err(|detail| {
                    AdapterError::InvalidConfig {
                        adapter: name.to_owned(),
                        detail: format!("capture {capture_name:?} has invalid JSONPath: {detail}"),
                    }
                })?;
            }
        }
    }
    if let Some(template) = &adapter.resume {
        for argument in template {
            for capture in placeholders(argument).map_err(|detail| AdapterError::InvalidConfig {
                adapter: name.to_owned(),
                detail: format!("invalid resume argument {argument:?}: {detail}"),
            })? {
                if capture != "cwd" && !names.contains(capture.as_str()) {
                    return invalid_config(
                        name,
                        format!("resume references unknown capture {capture:?}"),
                    );
                }
            }
        }
    }
    Ok(())
}

fn validate_launch_config(adapter: &str, launch: &AdapterLaunchConfig) -> Result<(), AdapterError> {
    if let Some(cwd_argv) = &launch.cwd_argv {
        validate_argv(adapter, "launch.cwdArgv", cwd_argv, false)?;
        if !cwd_argv.iter().any(|argument| argument.contains("%<cwd>%")) {
            return invalid_config(adapter, "launch.cwdArgv must reference %<cwd>%".to_owned());
        }
    }
    for (field, policies) in [
        ("approvalPolicies", &launch.approval_policies),
        ("sandboxPolicies", &launch.sandbox_policies),
    ] {
        for (policy, argv) in policies {
            if policy.trim().is_empty()
                || policy.contains('\0')
                || policy.chars().any(char::is_control)
            {
                return invalid_config(
                    adapter,
                    format!("launch.{field} has invalid policy name {policy:?}"),
                );
            }
            validate_argv(adapter, &format!("launch.{field}.{policy}"), argv, true)?;
        }
    }
    for policy in &launch.commit_capable_sandbox_policies {
        if !launch.sandbox_policies.contains_key(policy) {
            return invalid_config(
                adapter,
                format!(
                    "launch.commitCapableSandboxPolicies names {policy:?}, which launch.sandboxPolicies does not declare"
                ),
            );
        }
    }
    for (field, option) in [
        ("model", launch.model.as_ref()),
        ("effort", launch.effort.as_ref()),
    ] {
        let Some(option) = option else {
            continue;
        };
        validate_argv(
            adapter,
            &format!("launch.{field}.argv"),
            &option.argv,
            false,
        )?;
        if !option
            .argv
            .iter()
            .any(|argument| argument.contains("%<value>%"))
        {
            return invalid_config(
                adapter,
                format!("launch.{field}.argv must reference %<value>%"),
            );
        }
        if option.allowed_values.is_empty() {
            return invalid_config(
                adapter,
                format!("launch.{field}.allowedValues must not be empty"),
            );
        }
        let mut unique = BTreeSet::new();
        for value in &option.allowed_values {
            if value.trim().is_empty()
                || value.contains('\0')
                || value.chars().any(char::is_control)
                || !unique.insert(value)
            {
                return invalid_config(
                    adapter,
                    format!(
                        "launch.{field}.allowedValues must contain unique non-empty values without control characters"
                    ),
                );
            }
        }
    }
    Ok(())
}

fn validate_argv(
    adapter: &str,
    field: &str,
    argv: &[String],
    allow_empty: bool,
) -> Result<(), AdapterError> {
    if !allow_empty && argv.is_empty() {
        return invalid_config(adapter, format!("{field} must not be an empty argv"));
    }
    if argv.first().is_some_and(String::is_empty) {
        return invalid_config(adapter, format!("{field} executable must not be empty"));
    }
    if argv.iter().any(|value| value.contains('\0')) {
        return invalid_config(
            adapter,
            format!("{field} arguments must contain no NUL bytes"),
        );
    }
    Ok(())
}

fn invalid_config<T>(adapter: &str, detail: String) -> Result<T, AdapterError> {
    Err(AdapterError::InvalidConfig {
        adapter: adapter.to_owned(),
        detail,
    })
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_capture_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn placeholders(argument: &str) -> Result<Vec<String>, String> {
    let mut captures = Vec::new();
    let mut rest = argument;
    while let Some(start) = rest.find("%<") {
        rest = &rest[start + 2..];
        let end = rest
            .find(">%")
            .ok_or_else(|| "unclosed '%<captureName>%' placeholder".to_owned())?;
        let name = &rest[..end];
        if !valid_capture_name(name) {
            return Err(format!("invalid capture placeholder {name:?}"));
        }
        captures.push(name.to_owned());
        rest = &rest[end + 2..];
    }
    Ok(captures)
}

fn render_argument(
    adapter: &str,
    argument: &str,
    captures: &BTreeMap<String, Value>,
) -> Result<String, AdapterError> {
    let mut output = String::new();
    let mut rest = argument;
    while let Some(start) = rest.find("%<") {
        output.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest.find(">%").ok_or_else(|| AdapterError::InvalidConfig {
            adapter: adapter.to_owned(),
            detail: "unclosed '%<captureName>%' placeholder".to_owned(),
        })?;
        let capture = &rest[..end];
        if !valid_capture_name(capture) {
            return Err(AdapterError::InvalidConfig {
                adapter: adapter.to_owned(),
                detail: format!("invalid capture placeholder {capture:?}"),
            });
        }
        let value = captures
            .get(capture)
            .ok_or_else(|| AdapterError::MissingCapture {
                adapter: adapter.to_owned(),
                capture: capture.to_owned(),
            })?;
        match value {
            Value::String(value) => output.push_str(value),
            value => output.push_str(&serde_json::to_string(value).map_err(|error| {
                AdapterError::Scrape {
                    capture: capture.to_owned(),
                    detail: error.to_string(),
                }
            })?),
        }
        rest = &rest[end + 2..];
    }
    output.push_str(rest);
    Ok(output)
}

fn scrape_regex(pattern: &str, input: &str) -> Result<Option<Value>, String> {
    let expression = compile_regex(pattern).map_err(|error| error.to_string())?;
    let mut selected = None;
    for captures in expression.captures_iter(input) {
        let value = if captures.len() > 1 {
            captures
                .get(1)
                .ok_or_else(|| "the first regex capture group did not participate".to_owned())?
        } else {
            captures
                .get(0)
                .expect("a regex capture always contains the full match")
        };
        selected = Some(Value::String(value.as_str().to_owned()));
    }
    Ok(selected)
}

fn compile_regex(pattern: &str) -> Result<Regex, regex::Error> {
    RegexBuilder::new(pattern).multi_line(true).build()
}

fn parse_json_path(pattern: &str) -> Result<JsonPath, String> {
    JsonPath::parse(pattern).map_err(|error| error.to_string())
}

fn scrape_json_path(pattern: &str, input: &str) -> Result<Option<Value>, String> {
    let path = parse_json_path(pattern)?;
    let mut selected = None;
    let documents = serde_json::Deserializer::from_str(input).into_iter::<Value>();
    for (document_index, document) in documents.enumerate() {
        let document = match document {
            Ok(document) => document,
            Err(error) if error.is_eof() && document_index > 0 => break,
            Err(error) => return Err(format!("invalid JSON stream: {error}")),
        };
        for value in path.query(&document).all() {
            if !value.is_null() {
                selected = Some(value.clone());
            }
        }
    }
    Ok(selected)
}

fn scrape_json_path_last(pattern: &str, input: &str) -> Result<Option<Value>, String> {
    let path = parse_json_path(pattern)?;
    let documents = serde_json::Deserializer::from_str(input)
        .into_iter::<Value>()
        .enumerate()
        .map(|(document_index, document)| match document {
            Ok(document) => Ok(Some(document)),
            Err(error) if error.is_eof() && document_index > 0 => Ok(None),
            Err(error) => Err(format!("invalid JSON stream: {error}")),
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    Ok(path
        .query(&Value::Array(documents))
        .all()
        .into_iter()
        .rev()
        .find(|value| !value.is_null())
        .cloned())
}

fn read_capture(path: &Path) -> Result<String, AdapterError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        capture_read(
            path,
            io::Error::new(io::ErrorKind::InvalidInput, "capture path has no file name"),
        )
    })?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
        .map_err(|source| capture_read(path, source))?;
    let name = CString::new(name.as_bytes()).map_err(|_| {
        capture_read(
            path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "capture name contains a NUL byte",
            ),
        )
    })?;
    // The directory descriptor pins the validated capture directory while the
    // final component is opened. O_NONBLOCK makes a replaced FIFO/socket safe
    // to reject with fstat instead of blocking before the type check.
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if descriptor < 0 {
        return Err(capture_read(path, io::Error::last_os_error()));
    }
    // SAFETY: openat returned a new owned descriptor on the successful path.
    let file = unsafe { File::from_raw_fd(descriptor) };
    validate_capture_file(path, &file)?;
    let mut bytes = Vec::new();
    file.take(MAX_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| capture_read(path, source))?;
    if bytes.len() as u64 > MAX_CAPTURE_BYTES {
        return Err(AdapterError::CaptureTooLarge {
            path: path.display().to_string(),
            limit: MAX_CAPTURE_BYTES,
        });
    }
    String::from_utf8(bytes).map_err(|_| AdapterError::CaptureNotUtf8(path.display().to_string()))
}

fn validate_capture_file(path: &Path, file: &File) -> Result<(), AdapterError> {
    let metadata = file
        .metadata()
        .map_err(|source| capture_read(path, source))?;
    if !metadata.file_type().is_file() {
        return Err(AdapterError::CaptureNotRegular(path.display().to_string()));
    }
    if metadata.len() > MAX_CAPTURE_BYTES {
        return Err(AdapterError::CaptureTooLarge {
            path: path.display().to_string(),
            limit: MAX_CAPTURE_BYTES,
        });
    }
    Ok(())
}

fn capture_read(path: &Path, source: io::Error) -> AdapterError {
    AdapterError::CaptureRead {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    use super::*;

    fn adapter() -> AdapterConfig {
        AdapterConfig {
            argv: vec!["agent".to_owned(), "--batch".to_owned()],
            resume: Some(vec![
                "agent".to_owned(),
                "resume".to_owned(),
                "%<sessionRef>%".to_owned(),
                "--context=%<branch>%:%<attempt>%".to_owned(),
                "progress=100%".to_owned(),
            ]),
            scrape: BTreeMap::from([
                (
                    "attempt".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..attempt".to_owned(),
                    },
                ),
                (
                    "branch".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stderr,
                        mode: ScrapeMode::Regex,
                        pattern: "(?m)^branch=(.+)$".to_owned(),
                    },
                ),
                (
                    "model".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..model".to_owned(),
                    },
                ),
                (
                    "sessionRef".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..session_id".to_owned(),
                    },
                ),
                (
                    "usage".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..usage".to_owned(),
                    },
                ),
            ]),
            trace: None,
            yield_hook: Some(vec![
                "tally".to_owned(),
                "lease".to_owned(),
                "status".to_owned(),
            ]),
            env: BTreeMap::from([("AGENT_COLOR".to_owned(), "never".to_owned())]),
            launch: AdapterLaunchConfig::default(),
            hardening: AdapterHardening::None,
            extra_writable_paths: Vec::new(),
            skill_bundle: None,
            skill_revision: None,
            extra_config: BTreeMap::from([(
                "modelFlag".to_owned(),
                Value::String("--model".to_owned()),
            )]),
        }
    }

    fn engine(adapters: &BTreeMap<String, AdapterConfig>) -> AdapterEngine<'_> {
        AdapterEngine::new(adapters)
    }

    #[test]
    fn open_adapter_dispatches_direct_argv_and_multi_capture_resume() {
        let adapters = BTreeMap::from([("custom-nix-agent".to_owned(), adapter())]);
        let engine = engine(&adapters);
        engine.validate_all().unwrap();
        let workload = vec![
            "two words".to_owned(),
            "$(touch /tmp/nope)".to_owned(),
            String::new(),
        ];
        let launch = engine.launch("custom-nix-agent", &workload).unwrap();
        assert_eq!(
            launch.argv,
            ["agent", "--batch", "two words", "$(touch /tmp/nope)", ""]
        );
        assert_eq!(launch.env["AGENT_COLOR"], "never");
        assert_eq!(launch.hardening, AdapterHardening::None);
        assert_eq!(
            launch.yield_hook.as_deref(),
            Some(&["tally".to_owned(), "lease".to_owned(), "status".to_owned()][..])
        );

        let captures = ScrapeResult {
            captures: BTreeMap::from([
                ("attempt".to_owned(), Value::from(2)),
                ("branch".to_owned(), Value::String("feature/x".to_owned())),
                (
                    "sessionRef".to_owned(),
                    Value::String("sess two".to_owned()),
                ),
            ]),
        };
        let resumed = engine
            .resume("custom-nix-agent", &workload, &captures)
            .unwrap();
        assert_eq!(
            resumed.argv,
            [
                "agent",
                "resume",
                "sess two",
                "--context=feature/x:2",
                "progress=100%",
                "two words",
                "$(touch /tmp/nope)",
                "",
            ]
        );
    }

    #[test]
    fn hardening_name_defaults_to_none_and_propagates_without_directives() {
        let default: AdapterConfig = serde_json::from_value(serde_json::json!({
            "argv": ["agent"]
        }))
        .unwrap();
        assert_eq!(default.hardening, AdapterHardening::None);
        assert!(
            serde_json::to_value(&default)
                .unwrap()
                .get("hardening")
                .is_none(),
            "absent hardening must remain absent on compatibility serialization"
        );

        let strict: AdapterConfig = serde_json::from_value(serde_json::json!({
            "argv": ["agent"],
            "hardening": "strict"
        }))
        .unwrap();
        let adapters = BTreeMap::from([("strict-agent".to_owned(), strict)]);
        assert_eq!(
            engine(&adapters)
                .launch("strict-agent", &[])
                .unwrap()
                .hardening,
            AdapterHardening::Strict
        );

        let production: AdapterConfig = serde_json::from_value(serde_json::json!({
            "argv": ["agent"],
            "hardening": "production",
            "extraWritablePaths": ["/home/agent/.config/agent"]
        }))
        .unwrap();
        let adapters = BTreeMap::from([("production-agent".to_owned(), production)]);
        engine(&adapters).validate_all().unwrap();
        let invocation = engine(&adapters).launch("production-agent", &[]).unwrap();
        assert_eq!(invocation.hardening, AdapterHardening::Production);
        assert_eq!(
            invocation.extra_writable_paths,
            [PathBuf::from("/home/agent/.config/agent")]
        );

        let relative: AdapterConfig = serde_json::from_value(serde_json::json!({
            "argv": ["agent"],
            "hardening": "production",
            "extraWritablePaths": [".config/agent"]
        }))
        .unwrap();
        let adapters = BTreeMap::from([("relative-agent".to_owned(), relative)]);
        assert!(matches!(
            engine(&adapters).validate_all(),
            Err(AdapterError::InvalidConfig { detail, .. })
                if detail.contains("extraWritablePaths entry .config/agent must be absolute")
        ));
        assert!(serde_json::from_value::<AdapterConfig>(serde_json::json!({
            "argv": ["agent"],
            "hardening": "almost-strict"
        }))
        .is_err());
    }

    #[test]
    fn skill_revision_hashes_configured_bundle_bytes_or_uses_stable_identifier() {
        let bundle: AdapterConfig = serde_json::from_value(serde_json::json!({
            "argv": ["agent"],
            "skillBundle": "review protocol α\n"
        }))
        .unwrap();
        assert_eq!(
            bundle.resolved_skill_revision().as_deref(),
            Some("sha256:d4c012fb39d01ea2633ef9519d2bd1233837fad7cf216af85b6eb9fb7b86879d")
        );

        let identifier: AdapterConfig = serde_json::from_value(serde_json::json!({
            "argv": ["agent"],
            "skillRevision": "review-agent-v3"
        }))
        .unwrap();
        assert_eq!(
            identifier.resolved_skill_revision().as_deref(),
            Some("review-agent-v3")
        );

        let conflicting = BTreeMap::from([(
            "agent".to_owned(),
            serde_json::from_value(serde_json::json!({
                "argv": ["agent"],
                "skillBundle": "bundle",
                "skillRevision": "v1"
            }))
            .unwrap(),
        )]);
        assert!(matches!(
            engine(&conflicting).validate_all(),
            Err(AdapterError::InvalidConfig { detail, .. })
                if detail == "skillBundle and skillRevision are mutually exclusive"
        ));

        let legacy = serde_json::to_value(AdapterConfig::default()).unwrap();
        assert!(legacy.get("skillBundle").is_none());
        assert!(legacy.get("skillRevision").is_none());
    }

    #[test]
    fn json_stream_regex_stream_selection_and_verbatim_model_are_pinned() {
        let adapters = BTreeMap::from([("custom".to_owned(), adapter())]);
        let result = engine(&adapters)
            .scrape_text(
                "custom",
                concat!(
                    "{\"event\":{\"session_id\":\"session-1\",\"model\":\"OpenAI/GPT-X.Custom\",\"attempt\":1}}\n",
                    "{\"event\":{\"session_id\":\"session-1\",\"model\":\"OpenAI/GPT-X.Custom\",\"attempt\":2,\"usage\":{\"input_tokens\":17}}}\n"
                ),
                "branch=feature/adapter\n",
            )
            .unwrap();
        assert_eq!(result.session_ref().unwrap(), Some("session-1"));
        assert_eq!(result.model().unwrap(), Some("OpenAI/GPT-X.Custom"));
        assert_eq!(result.captures["attempt"], Value::from(2));
        assert_eq!(result.captures["branch"], "feature/adapter");
        assert_eq!(result.captures["usage"]["input_tokens"], 17);

        let interrupted = engine(&adapters)
            .scrape_text(
                "custom",
                "{\"event\":{\"session_id\":\"recoverable\"}}\n{\"event\":",
                "branch=feature/adapter\n",
            )
            .unwrap();
        assert_eq!(interrupted.session_ref().unwrap(), Some("recoverable"));
        assert!(engine(&adapters)
            .scrape_text(
                "custom",
                "{\"event\":{\"session_id\":\"bad\"}}\nnot-json\n{\"event\":",
                "branch=feature/adapter\n",
            )
            .is_err());

        assert_eq!(
            scrape_json_path(
                "$.events[?@.kind == 'selected'].values[1:]",
                r#"{"events":[{"kind":"ignored","values":[0,1]},{"kind":"selected","values":[2,3,4]}]}"#,
            )
            .unwrap(),
            Some(Value::from(4))
        );
    }

    #[test]
    fn regex_scraping_is_line_oriented_by_default() {
        assert_eq!(
            scrape_regex(
                "^TALLY_FINAL_MESSAGE=(.*)$",
                concat!(
                    "diagnostic output\n",
                    "TALLY_FINAL_MESSAGE={\"ok\":false}\n",
                    "TALLY_FINAL_MESSAGE={\"ok\":true,\"n\":3}\n"
                ),
            )
            .unwrap(),
            Some(Value::String("{\"ok\":true,\"n\":3}".to_owned()))
        );
    }

    #[test]
    fn json_path_last_selects_only_the_normative_final_agent_events() {
        let claude = concat!(
            "{\"type\":\"result\",\"result\":\"first\"}\n",
            "{\"type\":\"tool_result\",\"result\":\"ignore\"}\n",
            "{\"type\":\"result\",\"result\":\"final\"}\n",
        );
        assert_eq!(
            scrape_json_path_last("$[?@.type == 'result'].result", claude).unwrap(),
            Some(Value::String("final".to_owned()))
        );

        let codex = concat!(
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"first\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\",\"text\":\"ignore\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"final\"}}\n",
        );
        assert_eq!(
            scrape_json_path_last(
                "$[?@.type == 'item.completed' && @.item.type == 'agent_message'].item.text",
                codex,
            )
            .unwrap(),
            Some(Value::String("final".to_owned()))
        );

        let pi = concat!(
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"first\"}]}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ignore\"}]}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"final\"}]}}\n",
        );
        assert_eq!(
            scrape_json_path_last(
                "$[?@.type == 'message_end' && @.message.role == 'assistant'].message.content[?@.type == 'text'].text",
                pi,
            )
            .unwrap(),
            Some(Value::String("final".to_owned()))
        );
    }

    #[test]
    fn scrape_paths_reads_only_selected_private_capture_streams() {
        let temp = tempfile::tempdir().unwrap();
        let stdout = temp.path().join("job.out");
        let stderr = temp.path().join("job.err");
        fs::write(
            &stdout,
            "{\"event\":{\"session_id\":\"s\",\"model\":\"M\",\"attempt\":1,\"usage\":{}}}\n",
        )
        .unwrap();
        fs::write(&stderr, "branch=main\n").unwrap();
        let paths = ExecutionPaths {
            stdout,
            stderr,
            failure_stderr: temp.path().join("job.failure.err"),
            exit_record: PathBuf::from("unused"),
            capture_generation: PathBuf::from("unused"),
        };
        let adapters = BTreeMap::from([("custom".to_owned(), adapter())]);
        let result = engine(&adapters).scrape_paths("custom", &paths).unwrap();
        assert_eq!(result.captures["branch"], "main");
    }

    #[test]
    fn capture_reader_rejects_links_and_fifos_without_blocking() {
        let temp = tempfile::tempdir().unwrap();
        let regular = temp.path().join("regular");
        fs::write(&regular, "ok").unwrap();
        let link = temp.path().join("link");
        symlink(&regular, &link).unwrap();
        assert!(read_capture(&link).is_err());

        let fifo = temp.path().join("fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        assert!(matches!(
            read_capture(&fifo),
            Err(AdapterError::CaptureNotRegular(_))
        ));

        let real_parent = temp.path().join("real-parent");
        fs::create_dir(&real_parent).unwrap();
        fs::write(real_parent.join("job.out"), "ok").unwrap();
        let linked_parent = temp.path().join("linked-parent");
        symlink(&real_parent, &linked_parent).unwrap();
        assert!(read_capture(&linked_parent.join("job.out")).is_err());
    }

    #[test]
    fn invalid_templates_patterns_environment_and_reserved_capture_types_fail_closed() {
        let mut invalid = adapter();
        invalid.resume = Some(vec!["agent".to_owned(), "%<missing>%".to_owned()]);
        assert!(engine(&BTreeMap::from([("bad".to_owned(), invalid)]))
            .validate_all()
            .unwrap_err()
            .to_string()
            .contains("unknown capture"));

        let mut literal = adapter();
        literal.resume = Some(vec!["agent".to_owned(), "%sessionRef%".to_owned()]);
        let literal_adapters = BTreeMap::from([("literal".to_owned(), literal)]);
        engine(&literal_adapters).validate_all().unwrap();
        assert_eq!(
            engine(&literal_adapters)
                .resume("literal", &[], &ScrapeResult::default())
                .unwrap()
                .argv,
            ["agent", "%sessionRef%"]
        );

        let mut invalid = adapter();
        invalid.resume = Some(vec!["agent".to_owned(), "%<sessionRef".to_owned()]);
        assert!(engine(&BTreeMap::from([("bad".to_owned(), invalid)]))
            .validate_all()
            .unwrap_err()
            .to_string()
            .contains("unclosed"));

        let mut invalid = adapter();
        invalid.scrape.get_mut("branch").unwrap().pattern = "(".to_owned();
        assert!(engine(&BTreeMap::from([("bad".to_owned(), invalid)]))
            .validate_all()
            .is_err());

        let mut invalid = adapter();
        invalid
            .env
            .insert("TALLY_POOL".to_owned(), "stolen".to_owned());
        assert!(engine(&BTreeMap::from([("bad".to_owned(), invalid)]))
            .validate_all()
            .is_err());

        let result = ScrapeResult {
            captures: BTreeMap::from([("model".to_owned(), serde_json::json!({"name": "M"}))]),
        };
        assert!(matches!(
            result.model(),
            Err(AdapterError::ReservedCaptureType(name)) if name == "model"
        ));
    }

    #[test]
    fn missing_resume_capture_and_shell_without_resume_never_fresh_replay() {
        let adapters = BTreeMap::from([
            ("custom".to_owned(), adapter()),
            ("shell".to_owned(), AdapterConfig::default()),
        ]);
        let engine = engine(&adapters);
        engine.validate_all().unwrap();
        assert!(matches!(
            engine.resume("custom", &[], &ScrapeResult::default()),
            Err(AdapterError::MissingCapture { .. })
        ));
        assert!(matches!(
            engine.resume("shell", &["/bin/true".to_owned()], &ScrapeResult::default()),
            Err(AdapterError::NoResume(name)) if name == "shell"
        ));
        assert_eq!(
            engine
                .launch(
                    "shell",
                    &["/bin/echo".to_owned(), "literal;value".to_owned()]
                )
                .unwrap()
                .argv,
            ["/bin/echo", "literal;value"]
        );
    }

    #[test]
    fn codex_pre_prompt_cwd_policies_and_overrides_are_direct_and_resumable() {
        let codex = AdapterConfig {
            argv: vec![
                "codex".to_owned(),
                "exec".to_owned(),
                "--json".to_owned(),
                "--".to_owned(),
            ],
            resume: Some(vec![
                "codex".to_owned(),
                "-C".to_owned(),
                "%<cwd>%".to_owned(),
                "exec".to_owned(),
                "resume".to_owned(),
                "--json".to_owned(),
                "--model".to_owned(),
                "%<model>%".to_owned(),
                "%<sessionRef>%".to_owned(),
                "--".to_owned(),
            ]),
            scrape: BTreeMap::from([
                (
                    "model".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..model".to_owned(),
                    },
                ),
                (
                    "sessionRef".to_owned(),
                    ScrapeCapture {
                        stream: ScrapeStream::Stdout,
                        mode: ScrapeMode::JsonPath,
                        pattern: "$..thread_id".to_owned(),
                    },
                ),
            ]),
            launch: AdapterLaunchConfig {
                allow_pre_prompt_argv: true,
                cwd_argv: Some(vec!["-C".to_owned(), "%<cwd>%".to_owned()]),
                approval_policies: BTreeMap::from([("never".to_owned(), Vec::new())]),
                sandbox_policies: BTreeMap::from([("danger-full-access".to_owned(), Vec::new())]),
                commit_capable_sandbox_policies: BTreeSet::from(["danger-full-access".to_owned()]),
                model: Some(AdapterValueOverride {
                    argv: vec!["--model".to_owned(), "%<value>%".to_owned()],
                    allowed_values: vec!["gpt-5-codex".to_owned()],
                }),
                effort: Some(AdapterValueOverride {
                    argv: vec![
                        "-c".to_owned(),
                        "model_reasoning_effort=%<value>%".to_owned(),
                    ],
                    allowed_values: vec!["high".to_owned()],
                }),
            },
            ..AdapterConfig::default()
        };
        let adapters = BTreeMap::from([("codex".to_owned(), codex)]);
        let engine = engine(&adapters);
        engine.validate_all().unwrap();
        let options = AdapterJobOptions {
            pre_prompt_argv: vec!["--dangerously-bypass-approvals-and-sandbox".to_owned()],
            environment: BTreeMap::from([("NO_COLOR".to_owned(), "1".to_owned())]),
            approval_policy: Some("never".to_owned()),
            sandbox_policy: Some("danger-full-access".to_owned()),
            model: Some("gpt-5-codex".to_owned()),
            effort: Some("high".to_owned()),
        };
        let cwd = Path::new("/worktrees/issue-28");
        let launch = engine
            .launch_with_options("codex", &["author wave 3".to_owned()], &options, Some(cwd))
            .unwrap();
        assert_eq!(
            launch.argv,
            [
                "codex",
                "exec",
                "--json",
                "--dangerously-bypass-approvals-and-sandbox",
                "-C",
                "/worktrees/issue-28",
                "--model",
                "gpt-5-codex",
                "-c",
                "model_reasoning_effort=high",
                "--",
                "author wave 3",
            ]
        );
        assert_eq!(launch.env["NO_COLOR"], "1");

        let scraped = engine
            .scrape_text(
                "codex",
                "{\"type\":\"thread.started\",\"thread_id\":\"thread-28\",\"model\":\"gpt-5-codex\"}\n",
                "",
            )
            .unwrap();
        assert_eq!(scraped.session_ref().unwrap(), Some("thread-28"));
        let resumed = engine
            .resume_with_options(
                "codex",
                &["continue".to_owned()],
                &scraped,
                &options,
                Some(cwd),
            )
            .unwrap();
        assert_eq!(
            resumed.argv,
            [
                "codex",
                "-C",
                "/worktrees/issue-28",
                "exec",
                "resume",
                "--json",
                "--model",
                "gpt-5-codex",
                "thread-28",
                "--dangerously-bypass-approvals-and-sandbox",
                "-c",
                "model_reasoning_effort=high",
                "--",
                "continue",
            ]
        );
    }

    #[test]
    fn unconfigured_pre_prompt_and_unauthorized_values_fail_closed() {
        let adapters = BTreeMap::from([(
            "closed".to_owned(),
            AdapterConfig {
                argv: vec!["agent".to_owned(), "--".to_owned()],
                ..AdapterConfig::default()
            },
        )]);
        let options = AdapterJobOptions {
            pre_prompt_argv: vec!["--unsafe".to_owned()],
            ..AdapterJobOptions::default()
        };
        assert!(engine(&adapters)
            .launch_with_options("closed", &["work".to_owned()], &options, None)
            .unwrap_err()
            .to_string()
            .contains("not authorized"));
    }

    #[test]
    fn commit_capability_is_declared_against_real_sandbox_policies() {
        let launch = |commit_capable: &[&str]| AdapterLaunchConfig {
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
            ..AdapterLaunchConfig::default()
        };

        // An adapter that declares nothing cannot refuse anything.
        let silent = launch(&[]);
        assert!(!silent.declares_commit_capability());
        assert!(silent.permits_commit(None));
        assert!(silent.permits_commit(Some("workspace-write")));

        // An adapter that has spoken refuses the writable-but-not-committing
        // policy, and refuses the unnamed adapter default with it.
        let declared = launch(&["danger-full-access"]);
        assert!(declared.declares_commit_capability());
        assert!(declared.permits_commit(Some("danger-full-access")));
        assert!(!declared.permits_commit(Some("workspace-write")));
        assert!(!declared.permits_commit(None));
        assert_eq!(declared.commit_capable_names(), "danger-full-access");

        let adapters = BTreeMap::from([(
            "declared".to_owned(),
            AdapterConfig {
                argv: vec!["agent".to_owned(), "--".to_owned()],
                launch: declared,
                ..AdapterConfig::default()
            },
        )]);
        engine(&adapters).validate_all().unwrap();

        let dangling = BTreeMap::from([(
            "dangling".to_owned(),
            AdapterConfig {
                argv: vec!["agent".to_owned(), "--".to_owned()],
                launch: launch(&["read-only"]),
                ..AdapterConfig::default()
            },
        )]);
        assert!(engine(&dangling)
            .validate_all()
            .unwrap_err()
            .to_string()
            .contains("commitCapableSandboxPolicies"));
    }
}
