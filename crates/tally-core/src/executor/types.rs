use super::*;

pub const CAPTURE_DIRECTORY: &str = "capture";
pub const CAPTURE_ARCHIVE_DIRECTORY: &str = "capture/archive";
/// Where per-unit capture mutual-exclusion locks live.
///
/// A sibling of `unit-exit/`, deliberately not inside it: the `strict` and
/// `production` hardening presets grant a job write access to that whole
/// directory, because its `ExecStopPost` recorder writes the exit record there.
/// Those two presets grant the two current capture streams by name and no
/// capture directory at all, so a top-level sibling is outside both of them. A
/// lock the daemon may have to wait on must not be a file a job under a
/// narrowing preset can create, replace, or hold.
///
/// `workspace` and `none` are stated exceptions, not oversights. `workspace`
/// grants the whole state directory and `none` emits no `ReadWritePaths=` at
/// all, so a job under either can still reach this directory — the relocation
/// moves that surface, it does not remove it. Both presets are documented as
/// for trusted programs only, and
/// `executor::tests::hardening_presets_grant_the_capture_lock_directory_only_where_documented`
/// pins all four variants so the exception cannot become an accident.
pub const CAPTURE_LOCK_DIRECTORY: &str = "capture-lock";
/// Where capture locks lived before they moved to [`CAPTURE_LOCK_DIRECTORY`].
///
/// Nothing takes a lock here any more. The single remaining reader is the
/// retention sweep, which drains the historical population; keeping the old
/// location spelled exactly once stops the sweep and the relocation drifting
/// apart.
pub const LEGACY_CAPTURE_LOCK_DIRECTORY: &str = UNIT_EXIT_DIRECTORY;
pub const CAPTURE_LOCK_SUFFIX: &str = ".capture.lock";
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

    /// The unit name this identity would have carried before campaign task
    /// labels entered `unit_stem`.
    ///
    /// Nothing mints this name any more. It exists so a reader of a durable
    /// record written by an older binary can recognize the one historical
    /// naming scheme by construction instead of by pattern-matching a string,
    /// and so the migration that rewrites those records derives both halves of
    /// the rename from the same identity recovery derives its expectation from.
    pub fn pre_label_unit_name(&self) -> String {
        format!("tally-job-{}.service", self.unit_uuid())
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
    /// Source-compatibility placeholder for callers that still initialize the
    /// removed integration to `None`. It is neither serializable nor
    /// inhabitable, so no request can carry gate configuration through it.
    #[doc(hidden)]
    #[serde(skip)]
    pub git_ai: Option<std::convert::Infallible>,
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
    /// Present only after a terminal failure. This is the operator-facing,
    /// bounded UTF-8 `<uuid>.err` projection used by external monitors and
    /// failure diagnostics; raw bytes remain in `.adapter.err`.
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
    pub result_revision: Option<String>,
    /// Host that owned the child process. This is authoritative for remote
    /// execution and lets the coordinator stamp the worker hostname.
    pub host_id: Option<String>,
    /// Whether stdout/stderr for this exact generation are locally available
    /// for advisory adapter scraping.
    pub captures_available: bool,
}

/// Cgroup accounting properties read from a single `systemctl show` issued by
/// the exit recorder while the unit is still queryable (`ExecStopPost` runs
/// before the transient unit is garbage-collected). Every field is the raw
/// systemd property, in the units systemd reports it — nanoseconds and
/// microseconds — so a value that was never measured stays a typed absence
/// instead of a rounded, invented float. Seconds are derived at the point a
/// witness charge is built, never stored here.
///
/// `Eq` matters here, not just `PartialEq`: `LocalUnitFact` and its
/// containers derive it, and an `f64` field would silently make that
/// impossible to satisfy correctly (`NaN != NaN`). Keeping every field an
/// integer keeps the whole containment chain honestly comparable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UnitAccounting {
    /// `CPUUsageNSec`. Absent when `CPUAccounting=` is off for the unit, or
    /// when the probe itself failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_usage_nsec: Option<u64>,
    /// `ExecMainStartTimestampMonotonic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_main_start_monotonic_usec: Option<u64>,
    /// `ExecMainExitTimestampMonotonic`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_main_exit_monotonic_usec: Option<u64>,
}

impl UnitAccounting {
    /// CPU-seconds consumed by the unit's cgroup, the generic resource charge
    /// for any job regardless of which pool it ran in.
    #[must_use]
    pub fn cpu_seconds(self) -> Option<f64> {
        self.cpu_usage_nsec
            .map(|nsec| nsec as f64 / 1_000_000_000.0)
    }

    /// Wall-clock seconds the unit's main process actually ran
    /// (`ExecMainExitTimestampMonotonic − ExecMainStartTimestampMonotonic`),
    /// measured by systemd's own monotonic clock rather than the daemon's
    /// dispatch-side `Instant`. For a job that held a `vram`-resource pool
    /// this is "GPU-seconds" — but it is the main process's runtime, a
    /// **lower bound** on how long the job actually held the pool lease, not
    /// the lease span itself: the lease is held from admission through
    /// completion handling, which strictly contains this window. It is still
    /// the right quantity to prefer over CPU-cgroup time (which would
    /// understate a GPU job that is mostly waiting on the device by a much
    /// larger, unbounded margin), just not an exact occupancy figure.
    #[must_use]
    pub fn wall_seconds(self) -> Option<f64> {
        let start = self.exec_main_start_monotonic_usec?;
        let exit = self.exec_main_exit_monotonic_usec?;
        let usec = exit.checked_sub(start)?;
        Some(usec as f64 / 1_000_000.0)
    }
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
    /// Best-effort cgroup accounting, filled by the exit recorder from one
    /// `systemctl show` call. `None` covers both "the probe never ran" (a
    /// pre-#382 record) and "the probe ran and failed" — the failure is
    /// logged to the job's captured stderr at the point it happens, and this
    /// field never carries a value nobody measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting: Option<UnitAccounting>,
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
            return Err(ExecutorError::ExitRecordUnitMismatch {
                recorded: self.unit.clone(),
                expected: expected_unit.to_owned(),
            });
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
