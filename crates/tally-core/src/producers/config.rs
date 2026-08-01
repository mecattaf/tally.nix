use super::*;

pub const IN_SCOPE_PRODUCER_KINDS: &[&str] = &[
    "calendar",
    "events-dir",
    "gh",
    "build-effect",
    "pool-reachability",
];
pub const PRODUCER_RUNTIME_SCHEMA_VERSION: u32 = 1;

pub(super) const MAX_INGRESS_BYTES: u64 = 1024 * 1024;
pub(super) const INGRESS_SUFFIX: &str = ".producer.json";
pub(super) const MAX_PRODUCER_NAME_BYTES: usize = 96;
pub(super) const MAX_CLAIMABLE_NAME_BYTES: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerRuntimeRecord {
    pub schema_version: u32,
    pub producer: String,
    pub last_trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_emission: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_outcome: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

pub fn record_producer_runtime(
    state_dir: &Path,
    producer: &str,
    trigger: DateTime<Utc>,
    outcome: Option<Value>,
    error: Option<String>,
) -> Result<(), ProducerError> {
    validate_producer_name(producer)?;
    let emitted = outcome.as_ref().is_some_and(outcome_has_emission);
    let timestamp = trigger.to_rfc3339();
    let previous_emission = read_producer_runtime(state_dir, producer)
        .ok()
        .flatten()
        .and_then(|record| record.last_emission);
    let record = ProducerRuntimeRecord {
        schema_version: PRODUCER_RUNTIME_SCHEMA_VERSION,
        producer: producer.to_owned(),
        last_trigger: timestamp.clone(),
        last_emission: emitted.then_some(timestamp).or(previous_emission),
        last_outcome: outcome,
        last_error: error,
    };
    write_json_atomic(
        &state_dir
            .join("producers")
            .join(format!("{producer}.runtime.json")),
        &record,
    )
}

pub(super) fn outcome_has_emission(value: &Value) -> bool {
    match value {
        Value::String(path) => path.starts_with('/'),
        Value::Array(items) => items.iter().any(outcome_has_emission),
        Value::Object(fields) => {
            fields
                .get("enqueued")
                .and_then(Value::as_u64)
                .is_some_and(|count| count > 0)
                || fields.get("emitted").is_some_and(outcome_has_emission)
                || fields.get("ingress").is_some_and(outcome_has_emission)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

pub fn read_producer_runtime(
    state_dir: &Path,
    producer: &str,
) -> Result<Option<ProducerRuntimeRecord>, ProducerError> {
    validate_producer_name(producer)?;
    let path = state_dir
        .join("producers")
        .join(format!("{producer}.runtime.json"));
    if !path.exists() {
        return Ok(None);
    }
    let record: ProducerRuntimeRecord =
        serde_json::from_slice(&read_bounded_regular(&path, 256 * 1024)?)?;
    if record.schema_version != PRODUCER_RUNTIME_SCHEMA_VERSION || record.producer != producer {
        return Err(ProducerError::InvalidObservation(format!(
            "producer runtime state {} has an invalid identity or schema",
            path.display()
        )));
    }
    Ok(Some(record))
}

pub(super) fn default_adapter() -> String {
    "shell".to_owned()
}

pub(super) const fn default_priority() -> Priority {
    Priority::Low
}

pub(super) const fn default_poll_interval() -> u64 {
    60
}

pub(super) const fn default_probe_interval() -> u64 {
    30
}

pub(super) const fn default_hysteresis() -> u32 {
    3
}

pub(super) fn default_actor_exclude() -> String {
    "self".to_owned()
}

pub(super) const fn default_true() -> bool {
    true
}

pub(super) const fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerEnqueue {
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default = "default_adapter")]
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "AdapterJobOptions::is_default")]
    pub adapter_options: AdapterJobOptions,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_manifest: Option<GateManifestSpec>,
    /// Structured job input materialized in the daemon brief store and exposed
    /// as TALLY_BRIEF plus TALLY_BRIEF_HASH.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<Value>,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize",
        deserialize_with = "crate::poolset::deserialize"
    )]
    pub pools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default = "default_priority")]
    pub priority: Priority,
    #[serde(default)]
    pub dedup_key: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub evidence_class: Option<Value>,
    #[serde(default)]
    pub manifest_hash: Option<String>,
    #[serde(default)]
    pub consumption_estimate: Option<u64>,
    #[serde(default)]
    pub runtime_max_sec: Option<u64>,
    #[serde(default)]
    pub no_enqueue: bool,
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
}

impl ProducerEnqueue {
    pub(super) fn payload(
        &self,
        source: EnqueueSource,
        producer: Option<&str>,
        now: DateTime<Utc>,
        github: Option<&GhOrigin>,
    ) -> Result<EnqueuePayload, ProducerError> {
        let mut pools = self.pools.clone();
        crate::poolset::canonicalize(&mut pools).map_err(|error| {
            ProducerError::InvalidConfig(format!("producer enqueue has invalid pool set: {error}"))
        })?;
        let dedup_key = self
            .dedup_key
            .as_deref()
            .map(|key| expand_dedup_key(key, now))
            .transpose()?;
        let argv = self
            .argv
            .iter()
            .map(|argument| render_origin_template(argument, github))
            .collect::<Result<Vec<_>, _>>()?;
        let cwd = self
            .cwd
            .as_ref()
            .map(|path| {
                let path = path.to_str().ok_or_else(|| {
                    ProducerError::InvalidObservation(
                        "producer cwd template must be valid UTF-8".to_owned(),
                    )
                })?;
                let rendered = render_origin_template(path, github)?;
                let rendered = PathBuf::from(rendered);
                validate_resolved_path(&rendered, "producer cwd")?;
                Ok::<PathBuf, ProducerError>(rendered)
            })
            .transpose()?;
        let brief = self
            .brief
            .as_ref()
            .map(|brief| render_origin_value(brief, github))
            .transpose()?;
        if let Some(brief) = &brief {
            let rendered_bytes = serde_json::to_vec(brief)?.len() as u64;
            if rendered_bytes > crate::brief::MAX_BRIEF_BYTES {
                return Err(ProducerError::InvalidObservation(format!(
                    "rendered producer brief exceeds {} bytes",
                    crate::brief::MAX_BRIEF_BYTES
                )));
            }
        }
        Ok(EnqueuePayload {
            invocation: None,
            argv: Some(argv),
            pools: Some(pools),
            executor: self.executor.clone(),
            priority: Some(self.priority),
            adapter: Some(self.adapter.clone()),
            cwd,
            workspace: self.workspace.clone(),
            adapter_options: (!self.adapter_options.is_default())
                .then(|| self.adapter_options.clone()),
            gate_manifest: self.gate_manifest.clone(),
            brief,
            brief_path: None,
            resume_from: None,
            source: Some(source),
            dedup_key,
            submission: None,
            orchestration: None,
            parent: None,
            evidence: self.evidence.clone(),
            drv: None,
            evidence_class: self.evidence_class.clone(),
            manifest_hash: self.manifest_hash.clone(),
            consumption_estimate: self.consumption_estimate,
            runtime_max_sec: self.runtime_max_sec,
            no_enqueue: self.no_enqueue,
            credentials: self.credentials.clone(),
            origin: Some(match (producer, github) {
                (Some(name), Some(github)) => AdmissionOrigin::github(name, github.clone()),
                (Some(name), None) => AdmissionOrigin::producer(name, source),
                (None, _) => AdmissionOrigin::direct(source),
            }),
            caller_job_id: None,
            caller_job_token: None,
            gh_trigger_actor: None,
            gh_self_actor: None,
            gh_origin: None,
            task_uuid: None,
            related_trigger: None,
            wait: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CalendarProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    pub on_calendar: String,
    pub enqueue: ProducerEnqueue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EventsDirProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_sec: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhSourceItemKind {
    Issue,
    PullRequest,
}

impl GhSourceItemKind {
    pub(super) const fn matches(self, item_type: GhItemType) -> bool {
        matches!(
            (self, item_type),
            (Self::Issue, GhItemType::Issue) | (Self::PullRequest, GhItemType::PullRequest)
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhSourceConstraints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, alias = "repos")]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub owners: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<GhItemState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default)]
    pub kinds: Vec<GhSourceItemKind>,
    #[serde(default)]
    pub notification_reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, alias = "items")]
    pub item_allowlist: Vec<String>,
}

impl GhSourceConstraints {
    pub(super) fn has_identity_scope(&self) -> bool {
        self.repo.is_some()
            || !self.repositories.is_empty()
            || !self.owners.is_empty()
            || !self.item_allowlist.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhSource {
    Notifications(GhSourceConstraints),
    Search(GhSourceConstraints),
}

impl GhSource {
    pub(super) const fn kind(&self) -> &'static str {
        match self {
            Self::Notifications(_) => "notifications",
            Self::Search(_) => "search",
        }
    }

    pub(super) const fn constraints(&self) -> &GhSourceConstraints {
        match self {
            Self::Notifications(constraints) | Self::Search(constraints) => constraints,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhTriggers {
    #[serde(default)]
    pub command_comments: Vec<String>,
    #[serde(default)]
    pub mentions: Vec<String>,
    #[serde(default)]
    pub assignments: Vec<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    pub enable: bool,
    #[serde(default)]
    pub sources: Vec<GhSource>,
    #[serde(default)]
    pub triggers: GhTriggers,
    #[serde(default = "default_actor_exclude")]
    pub actor_exclude: String,
    #[serde(default)]
    pub allow_self_triggered: bool,
    #[serde(default)]
    pub allowed_actors: Vec<String>,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_sec: u64,
    #[serde(default = "default_true")]
    pub post_receipt: bool,
    #[serde(default)]
    pub post_evidence: bool,
    #[serde(default)]
    pub post_failure_evidence: bool,
    #[serde(default)]
    pub post_failure_stderr: bool,
    #[serde(default)]
    pub post_gate_summary: bool,
    #[serde(default)]
    pub request_review: bool,
    #[serde(default)]
    pub close_on_acceptance: bool,
    #[serde(default)]
    pub never_mutate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_on_pass: Option<bool>,
    pub enqueue: ProducerEnqueue,
}

impl GhProducer {
    /// Configurations serialized before `closeOnPass` existed retain the old
    /// fused behavior. Nix-rendered current configurations always include the
    /// field, so `false` is an explicit comment-only policy.
    pub fn close_on_pass(&self) -> bool {
        self.close_on_pass.unwrap_or(self.post_evidence)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BuildEffectWatch {
    #[default]
    GcRootsDir,
    Jsonl,
    PostBuildHook,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BuildEffectProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub watch: BuildEffectWatch,
    pub path: PathBuf,
    pub on_key: ProducerEnqueue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PoolReachabilityProducer {
    #[serde(default)]
    pub credentials: BTreeMap<String, PathBuf>,
    pub probe_pool: String,
    #[serde(default = "default_probe_interval")]
    pub interval_sec: u64,
    #[serde(default = "default_hysteresis")]
    pub hysteresis: u32,
    #[serde(default)]
    pub on_lost: Option<ProducerEnqueue>,
    #[serde(default)]
    pub on_return: Option<ProducerEnqueue>,
    #[serde(default)]
    pub on_return_attest: Option<ProducerEnqueue>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ProducerConfig {
    Calendar(CalendarProducer),
    EventsDir(EventsDirProducer),
    Gh(GhProducer),
    BuildEffect(BuildEffectProducer),
    PoolReachability(Box<PoolReachabilityProducer>),
}

impl ProducerConfig {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Calendar(_) => "calendar",
            Self::EventsDir(_) => "events-dir",
            Self::Gh(_) => "gh",
            Self::BuildEffect(_) => "build-effect",
            Self::PoolReachability(_) => "pool-reachability",
        }
    }

    pub(super) fn credentials(&self) -> &BTreeMap<String, PathBuf> {
        match self {
            Self::Calendar(config) => &config.credentials,
            Self::EventsDir(config) => &config.credentials,
            Self::Gh(config) => &config.credentials,
            Self::BuildEffect(config) => &config.credentials,
            Self::PoolReachability(config) => &config.credentials,
        }
    }
}
