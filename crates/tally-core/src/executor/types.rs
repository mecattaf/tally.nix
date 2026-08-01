use super::*;

pub const CAPTURE_DIRECTORY: &str = "capture";
pub const CAPTURE_ARCHIVE_DIRECTORY: &str = "capture/archive";
pub const UNIT_EXIT_DIRECTORY: &str = "unit-exit";
pub const UNIT_EXIT_SCHEMA_VERSION: u32 = 2;
pub(super) const OPTIONAL_TALLY_ENVIRONMENT: [&str; 15] = [
    "TALLY_TASK_UUID",
    "TALLY_TASK_REF",
    "TALLY_PARENT",
    "TALLY_NO_ENQUEUE",
    "TALLY_CREDENTIALS",
    "TALLY_YIELD_HOOK",
    "TALLY_SOCKET",
    "TALLY_JOB_TOKEN",
    "TALLY_WORKSPACE_REPO",
    "TALLY_WORKSPACE_BASE_REV",
    "TALLY_WORKSPACE_BRANCH",
    "TALLY_WORKSPACE_PATH",
    "TALLY_BRIEF",
    "TALLY_BRIEF_HASH",
    "TALLY_GATE_MANIFEST",
];
pub(super) const GH_TALLY_ENVIRONMENT: [&str; 11] = [
    "TALLY_GH_REPO",
    "TALLY_GH_NUMBER",
    "TALLY_GH_URL",
    "TALLY_GH_TYPE",
    "TALLY_GH_HEAD_SHA",
    "TALLY_GH_NODE_ID",
    "TALLY_GH_TRIGGER_KIND",
    "TALLY_GH_TRIGGER_ACTOR",
    "TALLY_GH_EVENT_ID",
    "TALLY_GH_COMMENT_ID",
    "TALLY_GH_CONTEXT",
];
pub(super) const GH_CONTEXT_DIRECTORY: &str = "github-context";
pub(super) const LAUNCH_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(60);

pub(super) static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionIdentity {
    pub job_id: Uuid,
    pub task_uuid: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
}

impl ExecutionIdentity {
    pub fn unit_uuid(&self) -> &Uuid {
        self.task_uuid.as_ref().unwrap_or(&self.job_id)
    }

    pub fn unit_stem(&self) -> String {
        self.task_ref.as_ref().map_or_else(
            || format!("tally-job-{}", self.unit_uuid()),
            |task_ref| {
                format!(
                    "tally-job-{}-{}-{}",
                    task_ref.campaign(),
                    task_ref.task_id(),
                    self.unit_uuid()
                )
            },
        )
    }

    pub fn unit_name(&self) -> String {
        format!("{}.service", self.unit_stem())
    }

    pub fn capture_stem(&self) -> String {
        self.task_ref.as_ref().map_or_else(
            || self.unit_uuid().to_string(),
            |task_ref| format!("{}.{}", self.unit_uuid(), task_ref.task_id()),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UnitLimits {
    pub cpu_weight: u16,
    pub memory_max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionRequest {
    pub identity: ExecutionIdentity,
    pub parent: Option<Uuid>,
    pub pools: Vec<String>,
    pub lease_epoch: u64,
    pub attempt: u32,
    pub priority: Priority,
    pub no_enqueue: bool,
    pub argv: Vec<String>,
    /// JSON-encoded as `TALLY_YIELD_HOOK` for a checkpoint-aware harness to
    /// parse and execute directly, without shell interpretation.
    pub yield_hook: Option<Vec<String>>,
    /// Daemon RPC endpoint used by checkpoint hooks. This is kept separate
    /// from adapter-controlled environment so it cannot be redirected there.
    pub tally_socket: Option<String>,
    /// Capability token minted by the coordinator for a local job generation.
    /// Remote executors never receive the coordinator's token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_token: Option<String>,
    pub environment: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gh_origin: Option<GhOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_path: Option<PathBuf>,
    /// Canonical brief content crosses the fixed remote-executor protocol only
    /// long enough for the worker to materialize its own content-addressed copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_document: Option<Value>,
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_manifest: Option<GateManifestSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ai: Option<GitAiExecution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_attestation: Option<ExecAttestationContext>,
    #[serde(default, skip_serializing_if = "AdapterHardening::is_none")]
    pub hardening: AdapterHardening,
    #[serde(default)]
    pub extra_writable_paths: Vec<PathBuf>,
    pub credentials: BTreeMap<String, PathBuf>,
    pub limits: UnitLimits,
    pub runtime_max_sec: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionPaths {
    pub stdout: PathBuf,
    /// The adapter's raw stderr stream. This remains available for declared
    /// scrapes and traces, but uses an explicit `.adapter.err` suffix so it is
    /// not mistaken for a terminal failure signal.
    pub stderr: PathBuf,
    /// Present only after a terminal failure. This is the operator-facing
    /// `<uuid>.err` capture used by external monitors and failure diagnostics.
    pub failure_stderr: PathBuf,
    pub exit_record: PathBuf,
    pub capture_generation: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedCapturePaths {
    pub stdout: PathBuf,
    /// Raw adapter stderr retained for scrape/trace reconstruction.
    pub stderr: PathBuf,
    /// Failure-only stderr capture, when this generation failed.
    pub failure_stderr: Option<PathBuf>,
    pub current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct CaptureGeneration {
    pub(super) attempt: u32,
    pub(super) lease_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionBackend {
    Systemd,
    Direct,
    Adopted,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "detail", rename_all = "kebab-case")]
pub enum ExecutionTermination {
    Exited(i32),
    Signaled {
        code: String,
        status: String,
    },
    RuntimeExceeded,
    ServiceFailed {
        service_result: String,
        exit_code: Option<String>,
        exit_status: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionOutcome {
    pub unit: String,
    pub backend: ExecutionBackend,
    pub paths: ExecutionPaths,
    pub record: UnitExitRecord,
    pub termination: ExecutionTermination,
    /// Canonical evidence computed on the worker that owns remote artifact
    /// paths. Local executions leave this unset and the daemon computes it.
    pub evidence_gate: Option<GateResult>,
    /// Structured execution/gate/acceptance facts computed on the filesystem
    /// that owns the declared gate manifest.
    pub semantic_completion: Option<SemanticCompletion>,
    /// Exact result/authorship binding computed on the host that owns the
    /// result worktree. Both remain absent when Git AI integration is disabled.
    pub result_revision: Option<String>,
    pub authorship: Option<Authorship>,
    pub authorship_sessions: Option<Vec<AuthorshipSession>>,
    /// Host that owned the child process. This is authoritative for remote
    /// execution and lets the coordinator stamp the worker hostname.
    pub host_id: Option<String>,
    /// Whether stdout/stderr for this exact generation are locally available
    /// for advisory adapter scraping.
    pub captures_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UnitExitRecord {
    pub schema_version: u32,
    pub unit: String,
    pub invocation_id: String,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub service_result: String,
    pub exit_code: Option<String>,
    pub exit_status: Option<String>,
}

impl UnitExitRecord {
    pub(crate) fn validate(&self, expected_unit: &str) -> Result<(), ExecutorError> {
        if self.schema_version != UNIT_EXIT_SCHEMA_VERSION {
            return Err(ExecutorError::InvalidExitRecord(format!(
                "unsupported schema version {}",
                self.schema_version
            )));
        }
        if self.unit != expected_unit {
            return Err(ExecutorError::InvalidExitRecord(format!(
                "record unit {:?} does not match expected unit {expected_unit:?}",
                self.unit
            )));
        }
        if self.attempt == 0 {
            return Err(ExecutorError::InvalidExitRecord(
                "attempt must be positive".to_owned(),
            ));
        }
        if self.lease_epoch == 0 {
            return Err(ExecutorError::InvalidExitRecord(
                "leaseEpoch must be positive".to_owned(),
            ));
        }
        for (name, value) in [
            ("invocationId", &self.invocation_id),
            ("serviceResult", &self.service_result),
        ] {
            if value.is_empty() {
                return Err(ExecutorError::InvalidExitRecord(format!(
                    "{name} must not be empty"
                )));
            }
        }
        if !matches!(
            self.service_result.as_str(),
            "success"
                | "protocol"
                | "timeout"
                | "exit-code"
                | "signal"
                | "core-dump"
                | "watchdog"
                | "exec-condition"
                | "oom-kill"
                | "start-limit-hit"
                | "resources"
        ) {
            return Err(ExecutorError::InvalidExitRecord(format!(
                "unknown serviceResult {:?}",
                self.service_result
            )));
        }
        match (&self.exit_code, &self.exit_status) {
            (None, None) => {
                if !matches!(
                    self.service_result.as_str(),
                    "protocol" | "start-limit-hit" | "resources"
                ) {
                    return Err(ExecutorError::InvalidExitRecord(format!(
                        "serviceResult {:?} requires exit metadata",
                        self.service_result
                    )));
                }
            }
            (Some(code), Some(status)) => {
                if status.is_empty() {
                    return Err(ExecutorError::InvalidExitRecord(
                        "exitStatus must not be empty when present".to_owned(),
                    ));
                }
                match code.as_str() {
                    "exited" => {
                        let status = status.parse::<u8>().map_err(|_| {
                            ExecutorError::InvalidExitRecord(format!(
                                "exitStatus {status:?} is not an exit code in 0..=255"
                            ))
                        })?;
                        if self.service_result == "exit-code" && status == 0 {
                            return Err(ExecutorError::InvalidExitRecord(
                                "serviceResult exit-code cannot carry exitStatus 0".to_owned(),
                            ));
                        }
                    }
                    "killed" | "dumped" => {
                        if !status.bytes().all(|byte| {
                            byte.is_ascii_uppercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'_' | b'+' | b'-')
                        }) {
                            return Err(ExecutorError::InvalidExitRecord(format!(
                                "signal exitStatus {status:?} has invalid characters"
                            )));
                        }
                    }
                    _ => {
                        return Err(ExecutorError::InvalidExitRecord(format!(
                            "unknown exitCode {code:?}"
                        )));
                    }
                }
            }
            _ => {
                return Err(ExecutorError::InvalidExitRecord(
                    "exitCode and exitStatus must both be present or both be null".to_owned(),
                ));
            }
        }
        let exit_code = self.exit_code.as_deref();
        let exit_status = self.exit_status.as_deref();
        let combination_is_valid = match self.service_result.as_str() {
            "success" => {
                matches!((exit_code, exit_status), (Some("exited"), Some("0")))
                    || exit_code == Some("killed")
            }
            "protocol" => {
                matches!(
                    (exit_code, exit_status),
                    (None, None) | (Some("exited"), Some("0"))
                )
            }
            "timeout" => matches!(exit_code, Some("exited") | Some("killed")),
            "exit-code" => exit_code == Some("exited"),
            "signal" => exit_code == Some("killed"),
            "core-dump" => exit_code == Some("dumped"),
            "watchdog" => matches!(exit_code, Some("exited") | Some("killed") | Some("dumped")),
            "exec-condition" => {
                exit_code == Some("exited")
                    && exit_status
                        .and_then(|status| status.parse::<u8>().ok())
                        .is_some_and(|status| (1..=254).contains(&status))
            }
            "oom-kill" => exit_code == Some("killed"),
            "start-limit-hit" => exit_code.is_none(),
            "resources" => true,
            _ => unreachable!("service result vocabulary was checked above"),
        };
        if !combination_is_valid {
            return Err(ExecutorError::InvalidExitRecord(format!(
                "serviceResult {:?} is inconsistent with exitCode {:?} and exitStatus {:?}",
                self.service_result, self.exit_code, self.exit_status
            )));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalUnitState {
    Absent,
    Running,
    Exited,
    InactiveWithoutRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LocalUnitFact {
    pub unit: String,
    pub loaded: bool,
    pub state: LocalUnitState,
    pub invocation_id: Option<String>,
    pub attempt: Option<u32>,
    pub lease_epoch: Option<u64>,
    pub exit_record: Option<UnitExitRecord>,
}

impl LocalUnitFact {
    pub fn absent(unit: impl Into<String>) -> Self {
        Self {
            unit: unit.into(),
            loaded: false,
            state: LocalUnitState::Absent,
            invocation_id: None,
            attempt: None,
            lease_epoch: None,
            exit_record: None,
        }
    }
}
