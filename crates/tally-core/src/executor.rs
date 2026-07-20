use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
pub use taskchampion::Uuid;
use thiserror::Error;
use tokio::process::Command;
use tokio::sync::watch;

use crate::config::Priority;

pub const CAPTURE_DIRECTORY: &str = "capture";
pub const UNIT_EXIT_DIRECTORY: &str = "unit-exit";
pub const UNIT_EXIT_SCHEMA_VERSION: u32 = 2;
const OPTIONAL_TALLY_ENVIRONMENT: [&str; 6] = [
    "TALLY_TASK_UUID",
    "TALLY_PARENT",
    "TALLY_NO_ENQUEUE",
    "TALLY_CREDENTIALS",
    "TALLY_YIELD_HOOK",
    "TALLY_SOCKET",
];

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionIdentity {
    pub job_id: Uuid,
    pub task_uuid: Option<Uuid>,
}

impl ExecutionIdentity {
    pub fn unit_uuid(&self) -> &Uuid {
        self.task_uuid.as_ref().unwrap_or(&self.job_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitLimits {
    pub cpu_weight: u16,
    pub memory_max_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub environment: BTreeMap<String, String>,
    pub cwd: Option<PathBuf>,
    pub credentials: BTreeMap<String, PathBuf>,
    pub limits: UnitLimits,
    pub runtime_max_sec: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPaths {
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub exit_record: PathBuf,
    pub capture_generation: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CaptureGeneration {
    attempt: u32,
    lease_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionBackend {
    Systemd,
    Direct,
    Adopted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub unit: String,
    pub backend: ExecutionBackend,
    pub paths: ExecutionPaths,
    pub record: UnitExitRecord,
    pub termination: ExecutionTermination,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalUnitState {
    Absent,
    Running,
    Exited,
    InactiveWithoutRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

fn validate_local_unit_fact_shape(
    expected_unit: &str,
    fact: &LocalUnitFact,
) -> Result<(), ExecutorError> {
    let invalid = |detail: String| ExecutorError::UnitProbe {
        unit: expected_unit.to_owned(),
        detail,
    };
    if fact.unit != expected_unit {
        return Err(invalid(format!(
            "probe returned unit {:?}, expected {expected_unit:?}",
            fact.unit
        )));
    }
    match fact.state {
        LocalUnitState::Absent => {
            if fact.loaded
                || fact.invocation_id.is_some()
                || fact.attempt.is_some()
                || fact.lease_epoch.is_some()
                || fact.exit_record.is_some()
            {
                return Err(invalid("absent unit carries execution metadata".to_owned()));
            }
        }
        LocalUnitState::Running => {
            if !fact.loaded
                || fact.invocation_id.as_deref().is_none_or(str::is_empty)
                || fact.attempt.is_none_or(|attempt| attempt == 0)
                || fact.lease_epoch.is_none_or(|epoch| epoch == 0)
                || fact.exit_record.is_some()
            {
                return Err(invalid(
                    "running unit has incomplete or contradictory metadata".to_owned(),
                ));
            }
        }
        LocalUnitState::Exited => {
            let record = fact
                .exit_record
                .as_ref()
                .ok_or_else(|| invalid("exited unit has no durable exit record".to_owned()))?;
            record.validate(expected_unit)?;
            if fact.invocation_id.as_deref() != Some(record.invocation_id.as_str())
                || fact.attempt != Some(record.attempt)
                || fact.lease_epoch != Some(record.lease_epoch)
            {
                return Err(invalid(
                    "exited unit metadata does not match its durable record".to_owned(),
                ));
            }
        }
        LocalUnitState::InactiveWithoutRecord => {
            if !fact.loaded
                || fact.invocation_id.as_deref().is_none_or(str::is_empty)
                || fact.attempt.is_some()
                || fact.lease_epoch.is_some()
                || fact.exit_record.is_some()
            {
                return Err(invalid(
                    "inactive unit has incomplete or contradictory metadata".to_owned(),
                ));
            }
        }
    }
    Ok(())
}

pub trait LocalUnitProbe: Send + Sync {
    fn inspect(&self, unit: &str, paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError>;
}

#[derive(Debug, Clone)]
pub struct SystemdLocalUnitProbe {
    systemctl: PathBuf,
}

impl Default for SystemdLocalUnitProbe {
    fn default() -> Self {
        Self {
            systemctl: PathBuf::from("systemctl"),
        }
    }
}

impl SystemdLocalUnitProbe {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            systemctl: program.into(),
        }
    }
}

impl LocalUnitProbe for SystemdLocalUnitProbe {
    fn inspect(&self, unit: &str, paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        let output = std::process::Command::new(&self.systemctl)
            .args([
                "--user",
                "show",
                "--property=LoadState",
                "--property=ActiveState",
                "--property=InvocationID",
                "--property=Environment",
                "--",
                unit,
            ])
            .output()
            .map_err(|source| ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: source.to_string(),
            })?;
        if !output.status.success() {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!(
                    "systemctl --user show failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            });
        }
        interpret_systemd_unit_show(unit, paths, &output.stdout)
    }
}

fn interpret_systemd_unit_show(
    unit: &str,
    paths: &ExecutionPaths,
    stdout: &[u8],
) -> Result<LocalUnitFact, ExecutorError> {
    let text = std::str::from_utf8(stdout).map_err(|error| ExecutorError::UnitProbe {
        unit: unit.to_owned(),
        detail: format!("systemctl show output is not UTF-8: {error}"),
    })?;
    let mut properties = HashMap::new();
    for line in text.lines() {
        let (name, value) = line
            .split_once('=')
            .ok_or_else(|| ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("malformed systemctl show line {line:?}"),
            })?;
        if !matches!(
            name,
            "LoadState" | "ActiveState" | "InvocationID" | "Environment"
        ) {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("unexpected systemctl show property {name:?}"),
            });
        }
        if properties.insert(name, value).is_some() {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("duplicate systemctl show property {name:?}"),
            });
        }
    }
    let required = |name: &'static str| {
        properties
            .get(name)
            .copied()
            .ok_or_else(|| ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("systemctl show omitted {name}"),
            })
    };
    let load_state = required("LoadState")?;
    let active_state = required("ActiveState")?;
    let invocation_id = required("InvocationID")?;
    let environment = required("Environment")?;
    let exit_record = match read_exit_record(&paths.exit_record, unit) {
        Ok(record) => Some(record),
        Err(error) if is_not_found(&error) => None,
        Err(error) => return Err(error),
    };

    if load_state == "not-found" {
        if active_state != "inactive" || !invocation_id.is_empty() || !environment.is_empty() {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!(
                    "not-found unit reported ActiveState={active_state:?}, InvocationID={invocation_id:?}, or a non-empty Environment"
                ),
            });
        }
        return match exit_record {
            Some(record) => Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: false,
                state: LocalUnitState::Exited,
                invocation_id: Some(record.invocation_id.clone()),
                attempt: Some(record.attempt),
                lease_epoch: Some(record.lease_epoch),
                exit_record: Some(record),
            }),
            None => Ok(LocalUnitFact::absent(unit)),
        };
    }
    if load_state != "loaded" {
        return Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: format!("unsupported LoadState {load_state:?}"),
        });
    }
    if invocation_id.is_empty() {
        return Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: "loaded unit has no InvocationID".to_owned(),
        });
    }
    if let Some(record) = exit_record {
        if record.invocation_id != invocation_id {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!(
                    "durable exit InvocationID {:?} does not match live unit InvocationID {invocation_id:?}",
                    record.invocation_id
                ),
            });
        }
        return Ok(LocalUnitFact {
            unit: unit.to_owned(),
            loaded: true,
            state: LocalUnitState::Exited,
            invocation_id: Some(invocation_id.to_owned()),
            attempt: Some(record.attempt),
            lease_epoch: Some(record.lease_epoch),
            exit_record: Some(record),
        });
    }
    match active_state {
        "active" | "activating" | "reloading" | "deactivating" => {
            let (attempt, lease_epoch) = execution_metadata_from_environment(unit, environment)?;
            Ok(LocalUnitFact {
                unit: unit.to_owned(),
                loaded: true,
                state: LocalUnitState::Running,
                invocation_id: Some(invocation_id.to_owned()),
                attempt: Some(attempt),
                lease_epoch: Some(lease_epoch),
                exit_record: None,
            })
        }
        "inactive" | "failed" => Ok(LocalUnitFact {
            unit: unit.to_owned(),
            loaded: true,
            state: LocalUnitState::InactiveWithoutRecord,
            invocation_id: Some(invocation_id.to_owned()),
            attempt: None,
            lease_epoch: None,
            exit_record: None,
        }),
        other => Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: format!("unsupported ActiveState {other:?}"),
        }),
    }
}

fn execution_metadata_from_environment(
    unit: &str,
    environment: &str,
) -> Result<(u32, u64), ExecutorError> {
    let words = split_systemd_words(environment).map_err(|detail| ExecutorError::UnitProbe {
        unit: unit.to_owned(),
        detail,
    })?;
    let mut attempt = None;
    let mut lease_epoch = None;
    for word in words {
        let Some((name, value)) = word.split_once('=') else {
            return Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: format!("malformed unit environment word {word:?}"),
            });
        };
        match name {
            "TALLY_ATTEMPT" => {
                let parsed = value
                    .parse::<u32>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: format!("invalid TALLY_ATTEMPT {value:?}"),
                    })?;
                if attempt.replace(parsed).is_some() {
                    return Err(ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: "duplicate TALLY_ATTEMPT".to_owned(),
                    });
                }
            }
            "TALLY_LEASE_EPOCH" => {
                let parsed = value
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value > 0)
                    .ok_or_else(|| ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: format!("invalid TALLY_LEASE_EPOCH {value:?}"),
                    })?;
                if lease_epoch.replace(parsed).is_some() {
                    return Err(ExecutorError::UnitProbe {
                        unit: unit.to_owned(),
                        detail: "duplicate TALLY_LEASE_EPOCH".to_owned(),
                    });
                }
            }
            _ => {}
        }
    }
    match (attempt, lease_epoch) {
        (Some(attempt), Some(lease_epoch)) => Ok((attempt, lease_epoch)),
        _ => Err(ExecutorError::UnitProbe {
            unit: unit.to_owned(),
            detail: "unit Environment omitted TALLY_ATTEMPT or TALLY_LEASE_EPOCH".to_owned(),
        }),
    }
}

fn split_systemd_words(input: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in input.chars() {
        if escaped {
            word.push(character);
            escaped = false;
            continue;
        }
        if character == '\\' {
            escaped = true;
            continue;
        }
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            character if character.is_whitespace() => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            other => word.push(other),
        }
    }
    if escaped || quote.is_some() {
        return Err("unterminated quoting in unit Environment".to_owned());
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("execution request is invalid: {0}")]
    InvalidRequest(String),
    #[error("executor I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("cannot serialize unit exit record: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot spawn {program}: {source}")]
    Spawn {
        program: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "systemd-run failed before a valid exit record was produced (status {status:?}): {stderr}"
    )]
    LauncherFailed { status: Option<i32>, stderr: String },
    #[error("direct fallback refuses credentialed jobs because LoadCredential is unavailable")]
    CredentialedFallback,
    #[error("unit exit record is invalid: {0}")]
    InvalidExitRecord(String),
    #[error("execution unit {0} is already reserved by another executor")]
    AlreadyRunning(String),
    #[error("cannot inspect local execution unit {unit}: {detail}")]
    UnitProbe { unit: String, detail: String },
    #[error("cannot reclaim local execution unit {unit}: {detail}")]
    UnitControl { unit: String, detail: String },
    #[error("local execution unit {unit} already exists in state {state:?}")]
    ExistingUnit { unit: String, state: LocalUnitState },
    #[error(
        "recovered execution unit {unit} became unavailable in state {state:?}; refusing replay"
    )]
    AdoptedUnitUnavailable { unit: String, state: LocalUnitState },
    #[error(
        "recovered execution unit {unit} changed invocation: expected {expected:?}, observed {observed:?}"
    )]
    AdoptedInvocationMismatch {
        unit: String,
        expected: String,
        observed: Option<String>,
    },
    #[error(
        "recovered execution unit {unit} has generation attempt={observed_attempt}, leaseEpoch={observed_lease_epoch}; expected attempt={expected_attempt}, leaseEpoch={expected_lease_epoch}"
    )]
    AdoptedGenerationMismatch {
        unit: String,
        expected_attempt: u32,
        expected_lease_epoch: u64,
        observed_attempt: u32,
        observed_lease_epoch: u64,
    },
    #[error("required ExecStopPost environment variable {0} is missing or non-Unicode")]
    MissingExitEnvironment(&'static str),
}

fn io_error(path: &Path, source: std::io::Error) -> ExecutorError {
    ExecutorError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Debug)]
struct UnitReservation {
    _file: File,
}

impl Drop for UnitReservation {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._file);
    }
}

#[derive(Clone)]
struct DirectProcess {
    pgid: i32,
    invocation_id: String,
    stopped: watch::Receiver<bool>,
}

struct DirectProcessGuard {
    key: Uuid,
    pgid: i32,
    registry: Arc<Mutex<HashMap<Uuid, DirectProcess>>>,
    stopped: watch::Sender<bool>,
    armed: bool,
}

impl DirectProcessGuard {
    fn mark_stopped(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut registry) = self.registry.lock() {
            if registry
                .get(&self.key)
                .is_some_and(|process| process.pgid == self.pgid)
            {
                registry.remove(&self.key);
            }
        }
        let _ = self.stopped.send(true);
        self.armed = false;
    }
}

impl Drop for DirectProcessGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Direct fallback is its own process group. This is also the unwind and
        // daemon-shutdown backstop, so descendants cannot outlive a dropped
        // execution future.
        let result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                eprintln!(
                    "tally: cannot kill direct process group {}: {error}",
                    self.pgid
                );
            }
        }
        self.mark_stopped();
    }
}

#[derive(Clone)]
pub struct Executor {
    state_dir: PathBuf,
    systemd_run: PathBuf,
    systemctl: PathBuf,
    recorder_program: PathBuf,
    unit_probe: Arc<dyn LocalUnitProbe>,
    direct_processes: Arc<Mutex<HashMap<Uuid, DirectProcess>>>,
    allow_direct_fallback: bool,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Executor")
            .field("state_dir", &self.state_dir)
            .field("systemd_run", &self.systemd_run)
            .field("systemctl", &self.systemctl)
            .field("recorder_program", &self.recorder_program)
            .finish_non_exhaustive()
    }
}

impl Executor {
    pub fn new(state_dir: impl Into<PathBuf>, recorder_program: impl Into<PathBuf>) -> Self {
        Self {
            state_dir: state_dir.into(),
            systemd_run: PathBuf::from("systemd-run"),
            systemctl: PathBuf::from("systemctl"),
            recorder_program: recorder_program.into(),
            unit_probe: Arc::new(SystemdLocalUnitProbe::default()),
            direct_processes: Arc::new(Mutex::new(HashMap::new())),
            allow_direct_fallback: true,
        }
    }

    pub fn with_systemd_run(mut self, program: impl Into<PathBuf>) -> Self {
        self.systemd_run = program.into();
        self
    }

    pub fn with_systemctl(mut self, program: impl Into<PathBuf>) -> Self {
        let program = program.into();
        self.systemctl = program.clone();
        self.unit_probe = Arc::new(SystemdLocalUnitProbe::with_program(program));
        self
    }

    pub fn with_unit_probe(mut self, probe: impl LocalUnitProbe + 'static) -> Self {
        self.unit_probe = Arc::new(probe);
        self
    }

    /// Require durable systemd ownership. The direct fallback remains available
    /// to the standalone executor/test surface, but a crash-survivable
    /// daemon must never launch work it cannot adopt or reclaim after SIGKILL.
    pub fn require_systemd(mut self) -> Self {
        self.allow_direct_fallback = false;
        self
    }

    pub fn unit_stem(&self, identity: &ExecutionIdentity) -> String {
        format!("tally-job-{}", identity.unit_uuid())
    }

    pub fn unit_name(&self, identity: &ExecutionIdentity) -> String {
        format!("{}.service", self.unit_stem(identity))
    }

    pub fn paths(&self, identity: &ExecutionIdentity) -> ExecutionPaths {
        let uuid = identity.unit_uuid();
        ExecutionPaths {
            stdout: self
                .state_dir
                .join(CAPTURE_DIRECTORY)
                .join(format!("{uuid}.out")),
            stderr: self
                .state_dir
                .join(CAPTURE_DIRECTORY)
                .join(format!("{uuid}.err")),
            exit_record: self
                .state_dir
                .join(UNIT_EXIT_DIRECTORY)
                .join(format!("{uuid}.json")),
            capture_generation: self
                .state_dir
                .join(UNIT_EXIT_DIRECTORY)
                .join(format!("{uuid}.capture.json")),
        }
    }

    pub fn capture_generation_matches(
        &self,
        identity: &ExecutionIdentity,
        attempt: u32,
        lease_epoch: u64,
    ) -> Result<bool, ExecutorError> {
        let path = self.paths(identity).capture_generation;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(io_error(&path, source)),
        };
        let metadata = file.metadata().map_err(|source| io_error(&path, source))?;
        if !metadata.file_type().is_file() || metadata.len() > 1024 {
            return Err(ExecutorError::InvalidRequest(format!(
                "capture generation {} is not a bounded regular file",
                path.display()
            )));
        }
        let generation: CaptureGeneration = serde_json::from_reader(file)?;
        Ok(generation.attempt == attempt && generation.lease_epoch == lease_epoch)
    }

    pub fn inspect_identity(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<LocalUnitFact, ExecutorError> {
        let unit = self.unit_name(identity);
        let fact = self.unit_probe.inspect(&unit, &self.paths(identity))?;
        validate_local_unit_fact_shape(&unit, &fact)?;
        Ok(fact)
    }

    pub async fn inspect_identity_async(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<LocalUnitFact, ExecutorError> {
        let executor = self.clone();
        let identity = identity.clone();
        let unit = self.unit_name(&identity);
        tokio::task::spawn_blocking(move || executor.inspect_identity(&identity))
            .await
            .map_err(|error| ExecutorError::UnitProbe {
                unit,
                detail: format!("unit probe worker failed: {error}"),
            })?
    }

    pub async fn reclaim_identity(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<(), ExecutorError> {
        self.reclaim_identity_exact(identity, None).await
    }

    pub async fn reclaim_identity_exact(
        &self,
        identity: &ExecutionIdentity,
        expected_invocation_id: Option<&str>,
    ) -> Result<(), ExecutorError> {
        for attempt in 0..=200 {
            let direct = self
                .direct_processes
                .lock()
                .map_err(|_| ExecutorError::UnitControl {
                    unit: self.unit_name(identity),
                    detail: "direct-process registry is poisoned".to_owned(),
                })?
                .get(identity.unit_uuid())
                .cloned();
            if let Some(mut direct) = direct {
                if let Some(expected) = expected_invocation_id {
                    if expected != direct.invocation_id {
                        return Err(ExecutorError::AdoptedInvocationMismatch {
                            unit: self.unit_name(identity),
                            expected: expected.to_owned(),
                            observed: Some(direct.invocation_id),
                        });
                    }
                }
                let result = unsafe { libc::kill(-direct.pgid, libc::SIGKILL) };
                if result != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(ExecutorError::UnitControl {
                            unit: self.unit_name(identity),
                            detail: format!(
                                "cannot kill direct process group {}: {error}",
                                direct.pgid
                            ),
                        });
                    }
                }
                while !*direct.stopped.borrow() {
                    direct
                        .stopped
                        .changed()
                        .await
                        .map_err(|_| ExecutorError::UnitControl {
                            unit: self.unit_name(identity),
                            detail: format!(
                                "direct process group {} lost its stop acknowledgement",
                                direct.pgid
                            ),
                        })?;
                }
                return Ok(());
            }

            let fact = self.inspect_identity_async(identity).await?;
            if let Some(expected) = expected_invocation_id {
                if fact.state != LocalUnitState::Absent
                    && fact.invocation_id.as_deref() != Some(expected)
                {
                    return Err(ExecutorError::AdoptedInvocationMismatch {
                        unit: fact.unit,
                        expected: expected.to_owned(),
                        observed: fact.invocation_id,
                    });
                }
            }
            if fact.state == LocalUnitState::Running {
                let mut command = Command::new(&self.systemctl);
                command
                    .kill_on_drop(true)
                    .args(["--user", "stop", "--", &fact.unit]);
                let output =
                    command
                        .output()
                        .await
                        .map_err(|source| ExecutorError::UnitControl {
                            unit: fact.unit.clone(),
                            detail: format!("cannot spawn {}: {source}", self.systemctl.display()),
                        })?;
                if !output.status.success() {
                    return Err(ExecutorError::UnitControl {
                        unit: fact.unit,
                        detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                    });
                }
                return Ok(());
            }
            let reserved = self.identity_is_reserved(identity).map_err(|error| {
                ExecutorError::UnitControl {
                    unit: fact.unit.clone(),
                    detail: format!("cannot verify execution reservation: {error}"),
                }
            })?;
            if !reserved {
                return Ok(());
            }
            if attempt == 200 {
                return Err(ExecutorError::UnitControl {
                    unit: fact.unit,
                    detail: "execution reservation is still held without a reclaimable unit"
                        .to_owned(),
                });
            }
            // The reservation is acquired before either backend becomes
            // externally visible. Give that bounded transition time to publish
            // a systemd unit or direct-process registry entry, then reclaim it.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        unreachable!("bounded reclaim loop always returns")
    }

    fn identity_is_reserved(&self, identity: &ExecutionIdentity) -> Result<bool, ExecutorError> {
        let exits = self
            .paths(identity)
            .exit_record
            .parent()
            .expect("exit path always has a parent")
            .to_owned();
        let lock_path = exits.join(format!("{}.lock", identity.unit_uuid()));
        let file = match OpenOptions::new()
            .create(false)
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(io_error(&lock_path, source)),
        };
        match file.try_lock_exclusive() {
            Ok(()) => Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(true),
            Err(source) => Err(io_error(&lock_path, source)),
        }
    }

    pub fn build_systemd_argv(
        &self,
        request: &ExecutionRequest,
    ) -> Result<Vec<OsString>, ExecutorError> {
        self.validate_request(request)?;
        let paths = self.paths(&request.identity);
        let unit_stem = self.unit_stem(&request.identity);
        let unit_name = self.unit_name(&request.identity);
        let exec_stop_post = self.exec_stop_post(&paths.exit_record, &unit_name)?;
        let mut args = vec![
            "--user".into(),
            "--wait".into(),
            "--collect".into(),
            "--unit".into(),
            unit_stem.into(),
            "--quiet".into(),
            "--expand-environment=no".into(),
        ];
        push_pair(&mut args, "--property", "Type=exec");
        push_pair(
            &mut args,
            "--property",
            format!("CPUWeight={}", request.limits.cpu_weight),
        );
        push_pair(
            &mut args,
            "--property",
            format!("MemoryMax={}", request.limits.memory_max_bytes),
        );
        if let Some(seconds) = request.runtime_max_sec {
            push_pair(&mut args, "--property", format!("RuntimeMaxSec={seconds}s"));
        }
        push_pair(
            &mut args,
            "--property",
            format!("StandardOutput=append:{}", display_path(&paths.stdout)?),
        );
        push_pair(
            &mut args,
            "--property",
            format!("StandardError=append:{}", display_path(&paths.stderr)?),
        );
        push_pair(
            &mut args,
            "--property",
            format!("ExecStopPost={exec_stop_post}"),
        );
        for (name, source) in &request.credentials {
            push_pair(
                &mut args,
                "--property",
                format!("LoadCredential={name}:{}", display_path(source)?),
            );
        }
        if let Some(cwd) = &request.cwd {
            push_pair(&mut args, "--working-directory", cwd.as_os_str());
        }
        for (name, value) in execution_environment(request)? {
            push_pair(&mut args, "--setenv", format!("{name}={value}"));
        }
        let unset_environment = environment_to_unset(request);
        if !unset_environment.is_empty() {
            push_pair(
                &mut args,
                "--property",
                format!("UnsetEnvironment={}", unset_environment.join(" ")),
            );
        }
        args.push("--".into());
        args.extend(request.argv.iter().map(OsString::from));
        Ok(args)
    }

    pub async fn execute(
        &self,
        request: ExecutionRequest,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        self.validate_request(&request)?;
        let observed = self.inspect_identity_async(&request.identity).await?;
        match observed.state {
            LocalUnitState::Absent => {}
            LocalUnitState::Exited => {
                let record = observed
                    .exit_record
                    .ok_or_else(|| ExecutorError::UnitProbe {
                        unit: observed.unit.clone(),
                        detail: "exited observation has no durable unit-exit record".to_owned(),
                    })?;
                record.validate(&observed.unit)?;
                if record.attempt == request.attempt && record.lease_epoch == request.lease_epoch {
                    let termination = classify_termination(&record)?;
                    return Ok(ExecutionOutcome {
                        unit: observed.unit,
                        backend: ExecutionBackend::Adopted,
                        paths: self.paths(&request.identity),
                        record,
                        termination,
                    });
                }
                if observed.loaded {
                    return Err(ExecutorError::ExistingUnit {
                        unit: observed.unit,
                        state: LocalUnitState::Exited,
                    });
                }
                let immediately_precedes = record
                    .attempt
                    .checked_add(1)
                    .is_some_and(|next| next == request.attempt)
                    && record.lease_epoch <= request.lease_epoch;
                if !immediately_precedes {
                    return Err(ExecutorError::ExistingUnit {
                        unit: observed.unit,
                        state: LocalUnitState::Exited,
                    });
                }
            }
            LocalUnitState::Running | LocalUnitState::InactiveWithoutRecord => {
                return Err(ExecutorError::ExistingUnit {
                    unit: observed.unit,
                    state: observed.state,
                });
            }
        }
        let _reservation = self.reserve(&request.identity)?;
        let paths = self.prepare_paths(&request.identity)?;
        write_capture_generation(
            &paths.capture_generation,
            CaptureGeneration {
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
            },
        )?;
        let args = self.build_systemd_argv(&request)?;
        let output = match Command::new(&self.systemd_run).args(&args).output().await {
            Ok(output) => output,
            Err(source)
                if source.kind() == std::io::ErrorKind::NotFound && self.allow_direct_fallback =>
            {
                return self.execute_direct(request, paths).await;
            }
            Err(source) => {
                return Err(ExecutorError::Spawn {
                    program: self.systemd_run.clone(),
                    source,
                });
            }
        };

        let unit = self.unit_name(&request.identity);
        let record = match read_exit_record(&paths.exit_record, &unit) {
            Ok(record) => record,
            Err(error) if is_not_found(&error) => {
                // Losing the systemd-run client must never release the caller's
                // lease while the exact transient unit may still be executing.
                self.reclaim_identity(&request.identity).await?;
                return Err(ExecutorError::LauncherFailed {
                    status: output.status.code(),
                    stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
                });
            }
            Err(error) => return Err(error),
        };
        let termination = classify_termination(&record)?;
        Ok(ExecutionOutcome {
            unit,
            backend: ExecutionBackend::Systemd,
            paths,
            record,
            termination,
        })
    }

    /// Consume the exact durable exit of an execution recovered as already
    /// running. Unlike `execute`, this path can never turn an absent or stale
    /// observation into a fresh launch.
    pub async fn adopt(
        &self,
        request: ExecutionRequest,
        expected_invocation_id: &str,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if request.attempt == 0 || request.lease_epoch == 0 || expected_invocation_id.is_empty() {
            return Err(ExecutorError::InvalidRequest(
                "adopted invocation, attempt, and lease epoch must be present".to_owned(),
            ));
        }
        loop {
            let observed = self.inspect_identity_async(&request.identity).await?;
            if observed.state != LocalUnitState::Absent
                && observed.invocation_id.as_deref() != Some(expected_invocation_id)
            {
                return Err(ExecutorError::AdoptedInvocationMismatch {
                    unit: observed.unit,
                    expected: expected_invocation_id.to_owned(),
                    observed: observed.invocation_id,
                });
            }
            match observed.state {
                LocalUnitState::Running => {
                    if observed.attempt != Some(request.attempt)
                        || observed.lease_epoch != Some(request.lease_epoch)
                    {
                        return Err(ExecutorError::AdoptedGenerationMismatch {
                            unit: observed.unit,
                            expected_attempt: request.attempt,
                            expected_lease_epoch: request.lease_epoch,
                            observed_attempt: observed.attempt.unwrap_or_default(),
                            observed_lease_epoch: observed.lease_epoch.unwrap_or_default(),
                        });
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                LocalUnitState::InactiveWithoutRecord => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                LocalUnitState::Absent => {
                    return Err(ExecutorError::AdoptedUnitUnavailable {
                        unit: observed.unit,
                        state: observed.state,
                    });
                }
                LocalUnitState::Exited => {
                    let record = observed
                        .exit_record
                        .ok_or_else(|| ExecutorError::UnitProbe {
                            unit: observed.unit.clone(),
                            detail: "exited observation has no durable unit-exit record".to_owned(),
                        })?;
                    record.validate(&observed.unit)?;
                    if record.attempt != request.attempt
                        || record.lease_epoch != request.lease_epoch
                    {
                        return Err(ExecutorError::AdoptedGenerationMismatch {
                            unit: observed.unit,
                            expected_attempt: request.attempt,
                            expected_lease_epoch: request.lease_epoch,
                            observed_attempt: record.attempt,
                            observed_lease_epoch: record.lease_epoch,
                        });
                    }
                    let termination = classify_termination(&record)?;
                    return Ok(ExecutionOutcome {
                        unit: observed.unit,
                        backend: ExecutionBackend::Adopted,
                        paths: self.paths(&request.identity),
                        record,
                        termination,
                    });
                }
            }
        }
    }

    fn validate_request(&self, request: &ExecutionRequest) -> Result<(), ExecutorError> {
        if !self.state_dir.is_absolute() {
            return Err(ExecutorError::InvalidRequest(
                "state directory must be absolute".to_owned(),
            ));
        }
        validate_systemd_path(&self.state_dir, "state directory")?;
        if !self.recorder_program.is_absolute() {
            return Err(ExecutorError::InvalidRequest(
                "exit recorder program must be absolute".to_owned(),
            ));
        }
        validate_systemd_path(&self.recorder_program, "exit recorder program")?;
        let mut canonical_pools = request.pools.clone();
        crate::poolset::canonicalize(&mut canonical_pools)
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        if canonical_pools != request.pools {
            return Err(ExecutorError::InvalidRequest(
                "pool set must be in canonical order".to_owned(),
            ));
        }
        if request.lease_epoch == 0 {
            return Err(ExecutorError::InvalidRequest(
                "lease epoch must be positive".to_owned(),
            ));
        }
        if request.attempt == 0 {
            return Err(ExecutorError::InvalidRequest(
                "attempt must be positive".to_owned(),
            ));
        }
        if request.argv.is_empty() || request.argv[0].is_empty() {
            return Err(ExecutorError::InvalidRequest(
                "argv must contain a non-empty executable".to_owned(),
            ));
        }
        if request.argv.iter().any(|argument| argument.contains('\0')) {
            return Err(ExecutorError::InvalidRequest(
                "argv must not contain NUL bytes".to_owned(),
            ));
        }
        if let Some(hook) = &request.yield_hook {
            if hook.is_empty() || hook[0].is_empty() {
                return Err(ExecutorError::InvalidRequest(
                    "yield hook must contain a non-empty executable".to_owned(),
                ));
            }
            if hook.iter().any(|argument| argument.contains('\0')) {
                return Err(ExecutorError::InvalidRequest(
                    "yield hook must not contain NUL bytes".to_owned(),
                ));
            }
        }
        if request
            .tally_socket
            .as_ref()
            .is_some_and(|socket| socket.is_empty() || socket.contains('\0'))
        {
            return Err(ExecutorError::InvalidRequest(
                "tally socket must be non-empty and contain no NUL bytes".to_owned(),
            ));
        }
        for (name, value) in &request.environment {
            if !valid_environment_name(name)
                || name.starts_with("TALLY_")
                || name == "CREDENTIALS_DIRECTORY"
            {
                return Err(ExecutorError::InvalidRequest(format!(
                    "adapter environment name {name:?} is invalid or reserved"
                )));
            }
            if value.contains('\0') {
                return Err(ExecutorError::InvalidRequest(format!(
                    "adapter environment {name:?} contains a NUL byte"
                )));
            }
        }
        if !(1..=10_000).contains(&request.limits.cpu_weight) {
            return Err(ExecutorError::InvalidRequest(
                "CPUWeight must be in 1..=10000".to_owned(),
            ));
        }
        if request.limits.memory_max_bytes == 0 || request.limits.memory_max_bytes == u64::MAX {
            return Err(ExecutorError::InvalidRequest(
                "MemoryMax must be positive and finite".to_owned(),
            ));
        }
        if let Some(seconds) = request.runtime_max_sec {
            if seconds == 0 || seconds >= u64::MAX / 1_000_000 {
                return Err(ExecutorError::InvalidRequest(
                    "runtimeMaxSec must be positive and fit systemd's microsecond range".to_owned(),
                ));
            }
        }
        if let Some(cwd) = &request.cwd {
            if !cwd.is_absolute() {
                return Err(ExecutorError::InvalidRequest(
                    "working directory must be absolute".to_owned(),
                ));
            }
            validate_systemd_path(cwd, "working directory")?;
        }
        for (name, source) in &request.credentials {
            validate_credential_name(name)?;
            if !source.is_absolute() {
                return Err(ExecutorError::InvalidRequest(format!(
                    "credential {name:?} source must be absolute"
                )));
            }
            validate_systemd_path(source, "credential source")?;
        }
        Ok(())
    }

    fn exec_stop_post(&self, record: &Path, unit: &str) -> Result<String, ExecutorError> {
        [
            self.recorder_program.as_os_str(),
            OsStr::new("__record-unit-exit"),
            OsStr::new("--record"),
            record.as_os_str(),
            OsStr::new("--unit"),
            OsStr::new(unit),
        ]
        .into_iter()
        .map(quote_systemd_exec_word)
        .collect::<Result<Vec<_>, _>>()
        .map(|words| format!(":{}", words.join(" ")))
    }

    fn reserve(&self, identity: &ExecutionIdentity) -> Result<UnitReservation, ExecutorError> {
        let paths = self.paths(identity);
        let exits = paths
            .exit_record
            .parent()
            .expect("exit path always has a parent");
        create_private_directory(exits)?;
        let lock_path = exits.join(format!("{}.lock", identity.unit_uuid()));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&lock_path)
            .map_err(|source| io_error(&lock_path, source))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| io_error(&lock_path, source))?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(UnitReservation { _file: file }),
            Err(source) if source.kind() == std::io::ErrorKind::WouldBlock => {
                Err(ExecutorError::AlreadyRunning(self.unit_name(identity)))
            }
            Err(source) => Err(io_error(&lock_path, source)),
        }
    }

    fn prepare_paths(&self, identity: &ExecutionIdentity) -> Result<ExecutionPaths, ExecutorError> {
        let paths = self.paths(identity);
        let capture = paths
            .stdout
            .parent()
            .expect("capture path always has a parent");
        let exits = paths
            .exit_record
            .parent()
            .expect("exit path always has a parent");
        create_private_directory(capture)?;
        create_private_directory(exits)?;
        create_private_file(&paths.stdout)?;
        create_private_file(&paths.stderr)?;
        match std::fs::remove_file(&paths.exit_record) {
            Ok(()) => sync_directory(exits)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_error(&paths.exit_record, source)),
        }
        Ok(paths)
    }

    async fn execute_direct(
        &self,
        request: ExecutionRequest,
        paths: ExecutionPaths,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        if !request.credentials.is_empty() {
            return Err(ExecutorError::CredentialedFallback);
        }
        let stdout = OpenOptions::new()
            .append(true)
            .open(&paths.stdout)
            .map_err(|source| io_error(&paths.stdout, source))?;
        let stderr = OpenOptions::new()
            .append(true)
            .open(&paths.stderr)
            .map_err(|source| io_error(&paths.stderr, source))?;
        let mut command = Command::new(&request.argv[0]);
        command
            .args(&request.argv[1..])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .process_group(0);
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        for name in environment_to_unset(&request) {
            command.env_remove(name);
        }
        for (name, value) in execution_environment(&request)? {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|source| ExecutorError::Spawn {
            program: PathBuf::from(&request.argv[0]),
            source,
        })?;
        let child_pid = child.id();
        let pgid = child_pid
            .and_then(|pid| i32::try_from(pid).ok())
            .ok_or_else(|| ExecutorError::UnitControl {
                unit: self.unit_name(&request.identity),
                detail: "direct child has no representable process-group id".to_owned(),
            })?;
        let (stopped, stopped_rx) = watch::channel(false);
        let key = *request.identity.unit_uuid();
        let invocation_id = format!("direct-{}", child_pid.unwrap_or(0));
        {
            let mut registry =
                self.direct_processes
                    .lock()
                    .map_err(|_| ExecutorError::UnitControl {
                        unit: self.unit_name(&request.identity),
                        detail: "direct-process registry is poisoned".to_owned(),
                    })?;
            if registry.contains_key(&key) {
                let _ = unsafe { libc::kill(-pgid, libc::SIGKILL) };
                return Err(ExecutorError::UnitControl {
                    unit: self.unit_name(&request.identity),
                    detail: "direct-process identity was already registered".to_owned(),
                });
            }
            registry.insert(
                key,
                DirectProcess {
                    pgid,
                    invocation_id: invocation_id.clone(),
                    stopped: stopped_rx,
                },
            );
        }
        let mut direct_guard = DirectProcessGuard {
            key,
            pgid,
            registry: self.direct_processes.clone(),
            stopped,
            armed: true,
        };
        let (record, termination) = if let Some(seconds) = request.runtime_max_sec {
            match tokio::time::timeout(Duration::from_secs(seconds), child.wait()).await {
                Ok(status) => direct_completion(
                    status.map_err(|source| ExecutorError::Spawn {
                        program: PathBuf::from(&request.argv[0]),
                        source,
                    })?,
                    invocation_id,
                ),
                Err(_) => {
                    terminate_direct_process_group(&mut child, child_pid)
                        .await
                        .map_err(|source| ExecutorError::Spawn {
                            program: PathBuf::from(&request.argv[0]),
                            source,
                        })?;
                    let record = UnitExitRecord {
                        schema_version: UNIT_EXIT_SCHEMA_VERSION,
                        unit: self.unit_name(&request.identity),
                        invocation_id,
                        attempt: request.attempt,
                        lease_epoch: request.lease_epoch,
                        service_result: "timeout".to_owned(),
                        exit_code: Some("killed".to_owned()),
                        exit_status: Some("KILL".to_owned()),
                    };
                    (record, ExecutionTermination::RuntimeExceeded)
                }
            }
        } else {
            direct_completion(
                child.wait().await.map_err(|source| ExecutorError::Spawn {
                    program: PathBuf::from(&request.argv[0]),
                    source,
                })?,
                invocation_id,
            )
        };
        // child.wait (or the timeout termination helper) has reaped the group
        // leader. Disarm before any exit-record I/O so PID reuse cannot make a
        // later Drop signal an unrelated process group.
        direct_guard.mark_stopped();
        let mut record = record;
        record.unit = self.unit_name(&request.identity);
        record.attempt = request.attempt;
        record.lease_epoch = request.lease_epoch;
        write_exit_record(&paths.exit_record, &record)?;
        Ok(ExecutionOutcome {
            unit: record.unit.clone(),
            backend: ExecutionBackend::Direct,
            paths,
            record,
            termination,
        })
    }
}

async fn terminate_direct_process_group(
    child: &mut tokio::process::Child,
    pid: Option<u32>,
) -> std::io::Result<()> {
    if let Some(pid) = pid.and_then(|value| i32::try_from(value).ok()) {
        // The child was spawned as its own process-group leader. A group kill prevents
        // descendants from outliving a direct-fallback runtime deadline.
        let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        child.wait().await?;
        return Ok(());
    }
    child.kill().await
}

fn push_pair(args: &mut Vec<OsString>, option: impl Into<OsString>, value: impl Into<OsString>) {
    args.push(option.into());
    args.push(value.into());
}

fn priority_name(priority: Priority) -> &'static str {
    match priority {
        Priority::Interrupt => "interrupt",
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

fn execution_environment(
    request: &ExecutionRequest,
) -> Result<Vec<(String, String)>, ExecutorError> {
    let mut environment = request
        .environment
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<Vec<_>>();
    environment.push((
        "TALLY_JOB_ID".to_owned(),
        request.identity.job_id.to_string(),
    ));
    if let Some(task_uuid) = &request.identity.task_uuid {
        environment.push(("TALLY_TASK_UUID".to_owned(), task_uuid.to_string()));
    }
    if let Some(parent) = &request.parent {
        environment.push(("TALLY_PARENT".to_owned(), parent.to_string()));
    }
    environment.extend([
        (
            "TALLY_POOL".to_owned(),
            crate::poolset::encoded(&request.pools)?,
        ),
        (
            "TALLY_LEASE_EPOCH".to_owned(),
            request.lease_epoch.to_string(),
        ),
        ("TALLY_ATTEMPT".to_owned(), request.attempt.to_string()),
        (
            "TALLY_CLASS".to_owned(),
            priority_name(request.priority).to_owned(),
        ),
    ]);
    if request.no_enqueue {
        environment.push(("TALLY_NO_ENQUEUE".to_owned(), "1".to_owned()));
    }
    if !request.credentials.is_empty() {
        let names = request.credentials.keys().collect::<Vec<_>>();
        environment.push((
            "TALLY_CREDENTIALS".to_owned(),
            serde_json::to_string(&names)?,
        ));
    }
    if let Some(hook) = &request.yield_hook {
        environment.push(("TALLY_YIELD_HOOK".to_owned(), serde_json::to_string(hook)?));
    }
    if let Some(socket) = &request.tally_socket {
        environment.push(("TALLY_SOCKET".to_owned(), socket.clone()));
    }
    Ok(environment)
}

fn valid_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn environment_to_unset(request: &ExecutionRequest) -> Vec<&'static str> {
    let mut names = Vec::new();
    if request.identity.task_uuid.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[0]);
    }
    if request.parent.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[1]);
    }
    if !request.no_enqueue {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[2]);
    }
    if request.credentials.is_empty() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[3]);
        names.push("CREDENTIALS_DIRECTORY");
    }
    if request.yield_hook.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[4]);
    }
    if request.tally_socket.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[5]);
    }
    names
}

fn validate_credential_name(name: &str) -> Result<(), ExecutorError> {
    let valid = !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(ExecutorError::InvalidRequest(format!(
            "invalid credential name {name:?}"
        )))
    }
}

fn display_path(path: &Path) -> Result<&str, ExecutorError> {
    path.to_str().ok_or_else(|| {
        ExecutorError::InvalidRequest(format!("path {} is not valid UTF-8", path.display()))
    })
}

fn validate_systemd_path(path: &Path, label: &str) -> Result<(), ExecutorError> {
    let path = display_path(path)?;
    if path.chars().any(char::is_control) {
        return Err(ExecutorError::InvalidRequest(format!(
            "{label} must not contain control characters"
        )));
    }
    if path.contains('%') {
        return Err(ExecutorError::InvalidRequest(format!(
            "{label} must not contain systemd specifier character %"
        )));
    }
    Ok(())
}

fn quote_systemd_exec_word(word: &OsStr) -> Result<String, ExecutorError> {
    let word = word.to_str().ok_or_else(|| {
        ExecutorError::InvalidRequest("ExecStopPost argument is not valid UTF-8".to_owned())
    })?;
    if word.chars().any(char::is_control) {
        return Err(ExecutorError::InvalidRequest(
            "ExecStopPost arguments must not contain control characters".to_owned(),
        ));
    }
    let mut quoted = String::with_capacity(word.len() + 2);
    quoted.push('"');
    for character in word.chars() {
        match character {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

fn create_private_directory(path: &Path) -> Result<(), ExecutorError> {
    std::fs::create_dir_all(path).map_err(|source| io_error(path, source))?;
    let metadata = std::fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_dir() {
        return Err(ExecutorError::InvalidRequest(format!(
            "private directory {} must not be a symbolic link",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error(path, source))
}

fn create_private_file(path: &Path) -> Result<(), ExecutorError> {
    replace_private_file(path, &[])
}

fn write_capture_generation(
    path: &Path,
    generation: CaptureGeneration,
) -> Result<(), ExecutorError> {
    replace_private_file(path, &serde_json::to_vec(&generation)?)
}

fn replace_private_file(path: &Path, contents: &[u8]) -> Result<(), ExecutorError> {
    let parent = path.parent().ok_or_else(|| {
        ExecutorError::InvalidRequest("private file path has no parent".to_owned())
    })?;
    create_private_directory(parent)?;
    let file_name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        ExecutorError::InvalidRequest("private file name is not valid UTF-8".to_owned())
    })?;
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|source| io_error(&temporary, source))?;
        file.write_all(contents)
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        std::fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn sync_directory(path: &Path) -> Result<(), ExecutorError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| io_error(path, source))
}

pub fn write_exit_record(path: &Path, record: &UnitExitRecord) -> Result<(), ExecutorError> {
    let parent = path.parent().ok_or_else(|| {
        ExecutorError::InvalidRequest("exit record path has no parent".to_owned())
    })?;
    create_private_directory(parent)?;
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path.file_name().and_then(OsStr::to_str).ok_or_else(|| {
        ExecutorError::InvalidRequest("exit record file name is not valid UTF-8".to_owned())
    })?;
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let mut renamed = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        let mut encoded = serde_json::to_vec(record)?;
        encoded.push(b'\n');
        file.write_all(&encoded)
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .map_err(|source| io_error(&temporary, source))?;
        std::fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        renamed = true;
        sync_directory(parent)
    })();
    if result.is_err() {
        if renamed {
            let _ = std::fs::remove_file(path);
            let _ = sync_directory(parent);
        } else {
            let _ = std::fs::remove_file(&temporary);
        }
    }
    result
}

pub fn read_exit_record(path: &Path, expected_unit: &str) -> Result<UnitExitRecord, ExecutorError> {
    let bytes = std::fs::read(path).map_err(|source| io_error(path, source))?;
    let record: UnitExitRecord = serde_json::from_slice(&bytes)?;
    record.validate(expected_unit)?;
    Ok(record)
}

pub fn persist_exit_record_from_env(
    path: &Path,
    expected_unit: &str,
) -> Result<UnitExitRecord, ExecutorError> {
    let mut values = HashMap::new();
    for name in [
        "INVOCATION_ID",
        "SERVICE_RESULT",
        "TALLY_ATTEMPT",
        "TALLY_LEASE_EPOCH",
    ] {
        let value = std::env::var(name).map_err(|_| ExecutorError::MissingExitEnvironment(name))?;
        values.insert(name, value);
    }
    for name in ["EXIT_CODE", "EXIT_STATUS"] {
        match std::env::var(name) {
            Ok(value) => {
                values.insert(name, value);
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(std::env::VarError::NotUnicode(_)) => {
                return Err(ExecutorError::MissingExitEnvironment(name));
            }
        }
    }
    persist_exit_record(path, expected_unit, &values)
}

fn persist_exit_record(
    path: &Path,
    expected_unit: &str,
    environment: &HashMap<&str, String>,
) -> Result<UnitExitRecord, ExecutorError> {
    let required = |name: &'static str| {
        environment
            .get(name)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or(ExecutorError::MissingExitEnvironment(name))
    };
    let optional = |name: &'static str| match environment.get(name) {
        Some(value) if value.is_empty() => Err(ExecutorError::InvalidExitRecord(format!(
            "{name} must not be empty when present"
        ))),
        Some(value) => Ok(Some(value.clone())),
        None => Ok(None),
    };
    let record = UnitExitRecord {
        schema_version: UNIT_EXIT_SCHEMA_VERSION,
        unit: expected_unit.to_owned(),
        invocation_id: required("INVOCATION_ID")?,
        attempt: required("TALLY_ATTEMPT")?.parse().map_err(|_| {
            ExecutorError::InvalidExitRecord("TALLY_ATTEMPT must be a positive u32".to_owned())
        })?,
        lease_epoch: required("TALLY_LEASE_EPOCH")?.parse().map_err(|_| {
            ExecutorError::InvalidExitRecord("TALLY_LEASE_EPOCH must be a positive u64".to_owned())
        })?,
        service_result: required("SERVICE_RESULT")?,
        exit_code: optional("EXIT_CODE")?,
        exit_status: optional("EXIT_STATUS")?,
    };
    record.validate(expected_unit)?;
    write_exit_record(path, &record)?;
    Ok(record)
}

fn classify_termination(record: &UnitExitRecord) -> Result<ExecutionTermination, ExecutorError> {
    if record.service_result == "timeout" {
        return Ok(ExecutionTermination::RuntimeExceeded);
    }
    if matches!(record.service_result.as_str(), "success" | "exit-code")
        && record.exit_code.as_deref() == Some("exited")
    {
        let status = record
            .exit_status
            .as_deref()
            .expect("validated exited records carry an exitStatus")
            .parse::<i32>()
            .map_err(|_| {
                ExecutorError::InvalidExitRecord(format!(
                    "exitStatus {:?} is not numeric for exitCode=exited",
                    record.exit_status
                ))
            })?;
        return Ok(ExecutionTermination::Exited(status));
    }
    match (
        record.service_result.as_str(),
        &record.exit_code,
        &record.exit_status,
    ) {
        ("success" | "signal" | "core-dump" | "oom-kill", Some(code), Some(status)) => {
            Ok(ExecutionTermination::Signaled {
                code: code.clone(),
                status: status.clone(),
            })
        }
        _ => Ok(ExecutionTermination::ServiceFailed {
            service_result: record.service_result.clone(),
            exit_code: record.exit_code.clone(),
            exit_status: record.exit_status.clone(),
        }),
    }
}

fn direct_completion(
    status: std::process::ExitStatus,
    invocation_id: String,
) -> (UnitExitRecord, ExecutionTermination) {
    if let Some(code) = status.code() {
        return (
            UnitExitRecord {
                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                unit: String::new(),
                invocation_id,
                attempt: 0,
                lease_epoch: 0,
                service_result: if code == 0 { "success" } else { "exit-code" }.to_owned(),
                exit_code: Some("exited".to_owned()),
                exit_status: Some(code.to_string()),
            },
            ExecutionTermination::Exited(code),
        );
    }
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal().unwrap_or(0)
    };
    #[cfg(not(unix))]
    let signal = 0;
    (
        UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: String::new(),
            invocation_id,
            attempt: 0,
            lease_epoch: 0,
            service_result: "signal".to_owned(),
            exit_code: Some("killed".to_owned()),
            exit_status: Some(signal.to_string()),
        },
        ExecutionTermination::Signaled {
            code: "killed".to_owned(),
            status: signal.to_string(),
        },
    )
}

fn is_not_found(error: &ExecutorError) -> bool {
    matches!(
        error,
        ExecutorError::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;

    use super::*;

    fn uuid(value: &str) -> Uuid {
        Uuid::parse_str(value).unwrap()
    }

    fn request() -> ExecutionRequest {
        ExecutionRequest {
            identity: ExecutionIdentity {
                job_id: uuid("00000000-0000-4000-8000-000000000001"),
                task_uuid: Some(uuid("00000000-0000-4000-8000-000000000002")),
            },
            parent: Some(uuid("00000000-0000-4000-8000-000000000003")),
            pools: vec!["gpu".to_owned()],
            lease_epoch: 7,
            attempt: 1,
            priority: Priority::High,
            no_enqueue: true,
            argv: vec![
                "/bin/leaf".to_owned(),
                "two words".to_owned(),
                "$HOME".to_owned(),
                "$(touch /tmp/nope);%n".to_owned(),
                "--option-looking".to_owned(),
            ],
            yield_hook: Some(vec![
                "tally".to_owned(),
                "lease".to_owned(),
                "status".to_owned(),
            ]),
            tally_socket: Some("/run/user/1000/tally.sock".to_owned()),
            environment: BTreeMap::from([("ADAPTER_COLOR".to_owned(), "never".to_owned())]),
            cwd: Some(PathBuf::from("/work tree")),
            credentials: BTreeMap::from([
                ("alpha".to_owned(), PathBuf::from("/run/keys/alpha")),
                ("zeta".to_owned(), PathBuf::from("/run/keys/zeta")),
            ]),
            limits: UnitLimits {
                cpu_weight: 250,
                memory_max_bytes: 1_073_741_824,
            },
            runtime_max_sec: Some(30),
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct AbsentProbe;

    impl LocalUnitProbe for AbsentProbe {
        fn inspect(
            &self,
            unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            Ok(LocalUnitFact::absent(unit))
        }
    }

    fn executor(state_dir: &Path) -> Executor {
        Executor::new(state_dir, "/nix/store/example/bin/tally").with_unit_probe(AbsentProbe)
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn systemd_argv_is_direct_stable_and_complete() {
        let request = request();
        let args = strings(
            &executor(Path::new("/state tree"))
                .build_systemd_argv(&request)
                .unwrap(),
        );
        assert_eq!(
            &args[..7],
            [
                "--user",
                "--wait",
                "--collect",
                "--unit",
                "tally-job-00000000-0000-4000-8000-000000000002",
                "--quiet",
                "--expand-environment=no",
            ]
        );
        for property in [
            "Type=exec",
            "CPUWeight=250",
            "MemoryMax=1073741824",
            "RuntimeMaxSec=30s",
            "StandardOutput=append:/state tree/capture/00000000-0000-4000-8000-000000000002.out",
            "StandardError=append:/state tree/capture/00000000-0000-4000-8000-000000000002.err",
            "LoadCredential=alpha:/run/keys/alpha",
            "LoadCredential=zeta:/run/keys/zeta",
        ] {
            assert!(args.windows(2).any(|pair| pair == ["--property", property]));
        }
        let exec_stop = args
            .windows(2)
            .find(|pair| pair[0] == "--property" && pair[1].starts_with("ExecStopPost="))
            .unwrap();
        assert!(exec_stop[1].starts_with("ExecStopPost=:"));
        assert!(exec_stop[1].contains("__record-unit-exit"));
        assert!(exec_stop[1].contains("/state tree/unit-exit/"));
        for environment in [
            "ADAPTER_COLOR=never",
            "TALLY_JOB_ID=00000000-0000-4000-8000-000000000001",
            "TALLY_TASK_UUID=00000000-0000-4000-8000-000000000002",
            "TALLY_PARENT=00000000-0000-4000-8000-000000000003",
            "TALLY_POOL=gpu",
            "TALLY_LEASE_EPOCH=7",
            "TALLY_ATTEMPT=1",
            "TALLY_CLASS=high",
            "TALLY_NO_ENQUEUE=1",
            "TALLY_CREDENTIALS=[\"alpha\",\"zeta\"]",
            "TALLY_YIELD_HOOK=[\"tally\",\"lease\",\"status\"]",
            "TALLY_SOCKET=/run/user/1000/tally.sock",
        ] {
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--setenv", environment]));
        }
        let separator = args.iter().rposition(|argument| argument == "--").unwrap();
        assert_eq!(&args[separator + 1..], request.argv);
        let joined = args.join("\n");
        for forbidden in ["DeviceMemoryMax", "Delegate=", "dmem", "servingSlice"] {
            assert!(!joined.contains(forbidden));
        }
    }

    #[test]
    fn rowless_identity_uses_job_uuid_and_optional_env_stays_absent() {
        let mut request = request();
        request.identity.task_uuid = None;
        request.parent = None;
        request.no_enqueue = false;
        request.credentials.clear();
        request.yield_hook = None;
        request.tally_socket = None;
        request.runtime_max_sec = None;
        request.cwd = None;
        let args = strings(
            &executor(Path::new("/state"))
                .build_systemd_argv(&request)
                .unwrap(),
        );
        assert!(args.contains(&"tally-job-00000000-0000-4000-8000-000000000001".to_owned()));
        let joined = args.join("\n");
        for absent in [
            "TALLY_TASK_UUID=",
            "TALLY_PARENT=",
            "TALLY_NO_ENQUEUE=",
            "TALLY_CREDENTIALS=",
            "TALLY_YIELD_HOOK=",
            "TALLY_SOCKET=",
            "RuntimeMaxSec=",
            "LoadCredential=",
        ] {
            assert!(!joined.contains(absent));
        }
        assert!(args.windows(2).any(|pair| {
            pair
                == [
                    "--property",
                    "UnsetEnvironment=TALLY_TASK_UUID TALLY_PARENT TALLY_NO_ENQUEUE TALLY_CREDENTIALS CREDENTIALS_DIRECTORY TALLY_YIELD_HOOK TALLY_SOCKET",
                ]
        }));
    }

    #[test]
    fn exec_stop_post_disables_environment_expansion() {
        let executor = Executor::new("/state", "/nix/store/$literal-path/bin/tally");
        let args = strings(&executor.build_systemd_argv(&request()).unwrap());
        let property = args
            .windows(2)
            .find(|pair| pair[0] == "--property" && pair[1].starts_with("ExecStopPost="))
            .unwrap();
        assert!(property[1].starts_with("ExecStopPost=:"));
        assert!(property[1].contains("$literal-path"));
        assert!(!property[1].contains("$$literal-path"));

        let specifier = Executor::new("/state", "/nix/store/%n/bin/tally");
        assert!(specifier.build_systemd_argv(&request()).is_err());
    }

    #[test]
    fn invalid_limits_runtime_paths_and_credentials_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let executor = executor(temp.path());
        let mut invalid = request();
        invalid.limits.cpu_weight = 0;
        assert!(executor.build_systemd_argv(&invalid).is_err());
        invalid = request();
        invalid.limits.memory_max_bytes = 0;
        assert!(executor.build_systemd_argv(&invalid).is_err());
        invalid = request();
        invalid.limits.memory_max_bytes = u64::MAX;
        assert!(executor.build_systemd_argv(&invalid).is_err());
        invalid = request();
        invalid.runtime_max_sec = Some(0);
        assert!(executor.build_systemd_argv(&invalid).is_err());
        invalid = request();
        invalid.runtime_max_sec = Some(u64::MAX / 1_000_000);
        assert!(executor.build_systemd_argv(&invalid).is_err());
        invalid = request();
        invalid.cwd = Some(PathBuf::from("relative"));
        assert!(executor.build_systemd_argv(&invalid).is_err());
        invalid = request();
        invalid.cwd = Some(PathBuf::from("/work/%n"));
        assert!(executor.build_systemd_argv(&invalid).is_err());
        invalid = request();
        invalid.credentials =
            BTreeMap::from([("secret".to_owned(), PathBuf::from("/run/keys/%n"))]);
        assert!(executor.build_systemd_argv(&invalid).is_err());
        for name in ["", ".", "..", "slash/name", "colon:name", "space name"] {
            invalid = request();
            invalid.credentials = BTreeMap::from([(name.to_owned(), PathBuf::from("/secret"))]);
            assert!(executor.build_systemd_argv(&invalid).is_err(), "{name:?}");
        }
        invalid = request();
        invalid
            .credentials
            .insert("x".repeat(256), PathBuf::from("/secret"));
        assert!(executor.build_systemd_argv(&invalid).is_err());
    }

    #[test]
    fn capture_files_truncate_and_exit_record_is_atomic_and_private() {
        let temp = tempfile::tempdir().unwrap();
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
        let request = request();
        let paths = executor.prepare_paths(&request.identity).unwrap();
        std::fs::write(&paths.stdout, b"stale-tail").unwrap();
        std::fs::write(&paths.stderr, b"stale-error").unwrap();
        let paths = executor.prepare_paths(&request.identity).unwrap();
        assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"");
        assert_eq!(std::fs::read(&paths.stderr).unwrap(), b"");
        assert_eq!(
            std::fs::metadata(&paths.stdout)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(paths.stdout.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let environment = HashMap::from([
            ("INVOCATION_ID", "abc123".to_owned()),
            ("SERVICE_RESULT", "success".to_owned()),
            ("TALLY_ATTEMPT", "1".to_owned()),
            ("TALLY_LEASE_EPOCH", "7".to_owned()),
            ("EXIT_CODE", "exited".to_owned()),
            ("EXIT_STATUS", "0".to_owned()),
        ]);
        let unit = executor.unit_name(&request.identity);
        persist_exit_record(&paths.exit_record, &unit, &environment).unwrap();
        let record = read_exit_record(&paths.exit_record, &unit).unwrap();
        assert_eq!(record.invocation_id, "abc123");
        assert_eq!(
            std::fs::metadata(&paths.exit_record)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let leftovers = std::fs::read_dir(paths.exit_record.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn capture_replacement_rejects_fifo_and_hardlink_truncation_attacks() {
        let temp = tempfile::tempdir().unwrap();
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
        let request = request();
        let paths = executor.prepare_paths(&request.identity).unwrap();
        std::fs::remove_file(&paths.stdout).unwrap();
        std::fs::remove_file(&paths.stderr).unwrap();

        let fifo = CString::new(paths.stdout.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let victim = temp.path().join("must-not-truncate");
        std::fs::write(&victim, b"preserved").unwrap();
        std::fs::hard_link(&victim, &paths.stderr).unwrap();
        std::fs::hard_link(&victim, &paths.capture_generation).unwrap();

        let replaced = executor.prepare_paths(&request.identity).unwrap();
        write_capture_generation(
            &replaced.capture_generation,
            CaptureGeneration {
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
            },
        )
        .unwrap();

        assert_eq!(std::fs::read(&victim).unwrap(), b"preserved");
        assert_eq!(std::fs::read(&replaced.stdout).unwrap(), b"");
        assert_eq!(std::fs::read(&replaced.stderr).unwrap(), b"");
        for path in [
            &replaced.stdout,
            &replaced.stderr,
            &replaced.capture_generation,
        ] {
            let metadata = std::fs::symlink_metadata(path).unwrap();
            assert!(metadata.file_type().is_file());
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
        assert_eq!(
            serde_json::from_slice::<CaptureGeneration>(
                &std::fs::read(&replaced.capture_generation).unwrap()
            )
            .unwrap(),
            CaptureGeneration {
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
            }
        );
    }

    #[test]
    fn duplicate_identity_is_reserved_before_capture_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
        let request = request();
        let first = executor.reserve(&request.identity).unwrap();
        assert!(matches!(
            executor.reserve(&request.identity),
            Err(ExecutorError::AlreadyRunning(_))
        ));
        drop(first);
        executor.reserve(&request.identity).unwrap();
    }

    #[derive(Debug, Clone)]
    struct FactProbe(LocalUnitFact);

    impl LocalUnitProbe for FactProbe {
        fn inspect(
            &self,
            _unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            Ok(self.0.clone())
        }
    }

    #[derive(Debug, Clone)]
    struct SequenceProbe(Arc<Mutex<std::collections::VecDeque<LocalUnitFact>>>);

    impl LocalUnitProbe for SequenceProbe {
        fn inspect(
            &self,
            _unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            self.0
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ExecutorError::InvalidRequest("probe sequence exhausted".to_owned()))
        }
    }

    #[derive(Debug, Clone, Copy)]
    struct FailingProbe;

    impl LocalUnitProbe for FailingProbe {
        fn inspect(
            &self,
            unit: &str,
            _paths: &ExecutionPaths,
        ) -> Result<LocalUnitFact, ExecutorError> {
            Err(ExecutorError::UnitProbe {
                unit: unit.to_owned(),
                detail: "fake probe failure".to_owned(),
            })
        }
    }

    #[tokio::test]
    async fn surviving_unit_and_probe_failure_stop_before_capture_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let base = executor(temp.path());
        let paths = base.prepare_paths(&request.identity).unwrap();
        std::fs::write(&paths.stdout, b"preserve-out").unwrap();
        std::fs::write(&paths.stderr, b"preserve-err").unwrap();
        let unit = base.unit_name(&request.identity);
        let running = LocalUnitFact {
            unit: unit.clone(),
            loaded: true,
            state: LocalUnitState::Running,
            invocation_id: Some("active-invocation".to_owned()),
            attempt: Some(request.attempt),
            lease_epoch: Some(request.lease_epoch),
            exit_record: None,
        };
        let guarded = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_unit_probe(FactProbe(running));
        assert!(matches!(
            guarded.execute(request.clone()).await,
            Err(ExecutorError::ExistingUnit {
                state: LocalUnitState::Running,
                ..
            })
        ));
        assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"preserve-out");
        assert_eq!(std::fs::read(&paths.stderr).unwrap(), b"preserve-err");

        let failed = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_unit_probe(FailingProbe);
        assert!(matches!(
            failed.execute(request).await,
            Err(ExecutorError::UnitProbe { .. })
        ));
        assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"preserve-out");
        assert_eq!(std::fs::read(&paths.stderr).unwrap(), b"preserve-err");
    }

    #[tokio::test]
    async fn matching_durable_exit_is_adopted_without_reexecution() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let base = executor(temp.path());
        let paths = base.prepare_paths(&request.identity).unwrap();
        std::fs::write(&paths.stdout, b"completed-once").unwrap();
        let unit = base.unit_name(&request.identity);
        let record = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: unit.clone(),
            invocation_id: "completed-invocation".to_owned(),
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        let fact = LocalUnitFact {
            unit,
            loaded: false,
            state: LocalUnitState::Exited,
            invocation_id: Some(record.invocation_id.clone()),
            attempt: Some(record.attempt),
            lease_epoch: Some(record.lease_epoch),
            exit_record: Some(record.clone()),
        };
        let guarded = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_unit_probe(FactProbe(fact));
        let outcome = guarded.execute(request).await.unwrap();
        assert_eq!(outcome.backend, ExecutionBackend::Adopted);
        assert_eq!(outcome.record, record);
        assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"completed-once");
    }

    #[tokio::test]
    async fn recovered_absence_fails_closed_without_replay_or_capture_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let base = executor(temp.path());
        let paths = base.prepare_paths(&request.identity).unwrap();
        std::fs::write(&paths.stdout, b"retained-output").unwrap();
        std::fs::write(&paths.stderr, b"retained-error").unwrap();

        assert!(matches!(
            base.adopt(request, "recovered-invocation").await,
            Err(ExecutorError::AdoptedUnitUnavailable {
                state: LocalUnitState::Absent,
                ..
            })
        ));
        assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"retained-output");
        assert_eq!(std::fs::read(&paths.stderr).unwrap(), b"retained-error");
    }

    #[tokio::test]
    async fn adoption_waits_through_exit_record_visibility_race() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = request();
        request.argv = vec![String::new(), "--raw-workload".to_owned()];
        let base = executor(temp.path());
        let paths = base.prepare_paths(&request.identity).unwrap();
        std::fs::write(&paths.stdout, b"retained-output").unwrap();
        let unit = base.unit_name(&request.identity);
        let record = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: unit.clone(),
            invocation_id: "recovered-invocation".to_owned(),
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        let facts = std::collections::VecDeque::from([
            LocalUnitFact {
                unit: unit.clone(),
                loaded: true,
                state: LocalUnitState::InactiveWithoutRecord,
                invocation_id: Some(record.invocation_id.clone()),
                attempt: None,
                lease_epoch: None,
                exit_record: None,
            },
            LocalUnitFact {
                unit,
                loaded: false,
                state: LocalUnitState::Exited,
                invocation_id: Some(record.invocation_id.clone()),
                attempt: Some(record.attempt),
                lease_epoch: Some(record.lease_epoch),
                exit_record: Some(record.clone()),
            },
        ]);
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_unit_probe(SequenceProbe(Arc::new(Mutex::new(facts))));
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            executor.adopt(request.clone(), "recovered-invocation"),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(outcome.record, record);
        assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"retained-output");

        let mut replacement = record;
        replacement.invocation_id = "replacement-invocation".to_owned();
        let replaced = Executor::new(temp.path(), "/nix/store/example/bin/tally").with_unit_probe(
            FactProbe(LocalUnitFact {
                unit: replacement.unit.clone(),
                loaded: false,
                state: LocalUnitState::Exited,
                invocation_id: Some(replacement.invocation_id.clone()),
                attempt: Some(replacement.attempt),
                lease_epoch: Some(replacement.lease_epoch),
                exit_record: Some(replacement),
            }),
        );
        assert!(matches!(
            replaced.adopt(request, "recovered-invocation").await,
            Err(ExecutorError::AdoptedInvocationMismatch { .. })
        ));
        assert_eq!(std::fs::read(&paths.stdout).unwrap(), b"retained-output");
    }

    #[tokio::test]
    async fn loaded_prior_exit_blocks_represent_before_capture_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = request();
        let base = executor(temp.path());
        let paths = base.prepare_paths(&request.identity).unwrap();
        std::fs::write(&paths.stdout, b"preserve-completed-out").unwrap();
        std::fs::write(&paths.stderr, b"preserve-completed-err").unwrap();
        let unit = base.unit_name(&request.identity);
        let record = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: unit.clone(),
            invocation_id: "prior-invocation".to_owned(),
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        write_exit_record(&paths.exit_record, &record).unwrap();
        let exit_before = std::fs::read(&paths.exit_record).unwrap();
        let fact = LocalUnitFact {
            unit,
            loaded: true,
            state: LocalUnitState::Exited,
            invocation_id: Some(record.invocation_id.clone()),
            attempt: Some(record.attempt),
            lease_epoch: Some(record.lease_epoch),
            exit_record: Some(record),
        };
        request.attempt += 1;
        request.lease_epoch += 1;

        let guarded = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_unit_probe(FactProbe(fact));
        assert!(matches!(
            guarded.execute(request).await,
            Err(ExecutorError::ExistingUnit {
                state: LocalUnitState::Exited,
                ..
            })
        ));
        assert_eq!(
            std::fs::read(&paths.stdout).unwrap(),
            b"preserve-completed-out"
        );
        assert_eq!(
            std::fs::read(&paths.stderr).unwrap(),
            b"preserve-completed-err"
        );
        assert_eq!(std::fs::read(&paths.exit_record).unwrap(), exit_before);
    }

    #[test]
    fn systemd_probe_executes_user_show_and_correlates_rowless_exit() {
        let temp = tempfile::tempdir().unwrap();
        let mut request = request();
        request.identity.task_uuid = None;
        let probe_program = temp.path().join("fake-systemctl");
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_systemctl(&probe_program);
        let unit = executor.unit_name(&request.identity);
        let expected_script = format!(
            "#!/bin/sh\n\
             [ \"$#\" -eq 8 ] || exit 81\n\
             [ \"$1\" = --user ] || exit 82\n\
             [ \"$2\" = show ] || exit 83\n\
             [ \"$3\" = --property=LoadState ] || exit 84\n\
             [ \"$4\" = --property=ActiveState ] || exit 85\n\
             [ \"$5\" = --property=InvocationID ] || exit 86\n\
             [ \"$6\" = --property=Environment ] || exit 87\n\
             [ \"$7\" = -- ] || exit 88\n\
             [ \"$8\" = {unit} ] || exit 89\n\
             printf 'LoadState=not-found\\nActiveState=inactive\\nInvocationID=\\nEnvironment=\\n'\n"
        );
        std::fs::write(&probe_program, expected_script).unwrap();
        let mut permissions = std::fs::metadata(&probe_program).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&probe_program, permissions).unwrap();

        let absent = executor.inspect_identity(&request.identity).unwrap();
        assert_eq!(absent, LocalUnitFact::absent(&unit));
        assert!(unit.contains(request.identity.job_id.to_string().as_str()));

        let paths = executor.prepare_paths(&request.identity).unwrap();
        let record = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: unit.clone(),
            invocation_id: "durable-invocation".to_owned(),
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        write_exit_record(&paths.exit_record, &record).unwrap();
        let loaded_script = format!(
            "#!/bin/sh\n\
             [ \"$#\" -eq 8 ] || exit 81\n\
             [ \"$8\" = {unit} ] || exit 89\n\
             printf 'LoadState=loaded\\nActiveState=inactive\\nInvocationID=durable-invocation\\nEnvironment=\\n'\n"
        );
        let loaded_probe_program = temp.path().join("fake-systemctl-loaded");
        std::fs::write(&loaded_probe_program, loaded_script).unwrap();
        let mut permissions = std::fs::metadata(&loaded_probe_program)
            .unwrap()
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&loaded_probe_program, permissions).unwrap();
        let loaded_executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_systemctl(&loaded_probe_program);
        let exited = loaded_executor.inspect_identity(&request.identity).unwrap();
        assert!(exited.loaded);
        assert_eq!(exited.state, LocalUnitState::Exited);
        assert_eq!(exited.exit_record, Some(record));

        let failed_probe_program = temp.path().join("fake-systemctl-failed");
        std::fs::write(&failed_probe_program, "#!/bin/sh\nexit 23\n").unwrap();
        let mut permissions = std::fs::metadata(&failed_probe_program)
            .unwrap()
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&failed_probe_program, permissions).unwrap();
        let failed_executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_systemctl(&failed_probe_program);
        assert!(matches!(
            failed_executor.inspect_identity(&request.identity),
            Err(ExecutorError::UnitProbe { .. })
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn async_unit_probe_keeps_current_thread_timers_live() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let systemctl = temp.path().join("slow-systemctl");
        std::fs::write(
            &systemctl,
            "#!/bin/sh\nsleep 1\nprintf 'LoadState=not-found\\nActiveState=inactive\\nInvocationID=\\nEnvironment=\\n'\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&systemctl).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&systemctl, permissions).unwrap();
        let executor =
            Executor::new(temp.path(), "/nix/store/example/bin/tally").with_systemctl(systemctl);
        let probe = executor.inspect_identity_async(&request.identity);
        tokio::pin!(probe);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            result = &mut probe => panic!("slow probe completed unexpectedly: {result:?}"),
        }
        assert_eq!(probe.await.unwrap().state, LocalUnitState::Absent);
    }

    #[tokio::test]
    async fn hard_reclaim_stops_the_exact_running_unit_and_fails_closed() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let control = temp.path().join("fake-systemctl-stop");
        let marker = temp.path().join("stopped-unit");
        let base =
            Executor::new(temp.path(), "/nix/store/example/bin/tally").with_systemctl(&control);
        let unit = base.unit_name(&request.identity);
        let script = format!(
            "#!/bin/sh\n\
             [ \"$#\" -eq 4 ] || exit 81\n\
             [ \"$1\" = --user ] || exit 82\n\
             [ \"$2\" = stop ] || exit 83\n\
             [ \"$3\" = -- ] || exit 84\n\
             printf '%s' \"$4\" > {}\n",
            marker.display()
        );
        std::fs::write(&control, script).unwrap();
        let mut permissions = std::fs::metadata(&control).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&control, permissions).unwrap();
        let running = LocalUnitFact {
            unit: unit.clone(),
            loaded: true,
            state: LocalUnitState::Running,
            invocation_id: Some("running-invocation".to_owned()),
            attempt: Some(request.attempt),
            lease_epoch: Some(request.lease_epoch),
            exit_record: None,
        };
        let executor = base.with_unit_probe(FactProbe(running.clone()));
        executor.reclaim_identity(&request.identity).await.unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), unit);
        std::fs::remove_file(&marker).unwrap();
        assert!(matches!(
            executor
                .reclaim_identity_exact(&request.identity, Some("prior-invocation"))
                .await,
            Err(ExecutorError::AdoptedInvocationMismatch { .. })
        ));
        assert!(!marker.exists(), "replacement invocation was stopped");

        let failing_control = temp.path().join("fake-systemctl-stop-failing");
        std::fs::write(&failing_control, "#!/bin/sh\nexit 23\n").unwrap();
        let mut permissions = std::fs::metadata(&failing_control).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&failing_control, permissions).unwrap();
        std::fs::rename(&failing_control, &control).unwrap();
        assert!(matches!(
            executor.reclaim_identity(&request.identity).await,
            Err(ExecutorError::UnitControl { .. })
        ));

        let missing_control = temp.path().join("missing-systemctl");
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_systemctl(&missing_control)
            .with_unit_probe(FactProbe(running));
        assert!(matches!(
            executor.reclaim_identity(&request.identity).await,
            Err(ExecutorError::UnitControl {
                unit: failed_unit,
                ..
            }) if failed_unit == unit
        ));
    }

    #[tokio::test]
    async fn hard_reclaim_kills_and_awaits_a_direct_process_group() {
        let temp = tempfile::tempdir().unwrap();
        let started = temp.path().join("direct-started");
        let descendant_started = temp.path().join("descendant-started");
        let leaked = temp.path().join("descendant-leaked");
        let mut request = fixture_request("fixture-reclaim");
        request.cwd = Some(temp.path().to_owned());
        let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_systemd_run(temp.path().join("missing-systemd-run"))
            .with_unit_probe(FactProbe(LocalUnitFact::absent(&unit)));
        let running_executor = executor.clone();
        let running_request = request.clone();
        let running = tokio::spawn(async move { running_executor.execute(running_request).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !started.exists() || !descendant_started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();

        executor.reclaim_identity(&request.identity).await.unwrap();
        let outcome = running.await.unwrap().unwrap();
        assert!(matches!(
            outcome.termination,
            ExecutionTermination::Signaled { .. }
        ));
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(!leaked.exists(), "direct descendant survived hard reclaim");
    }

    #[tokio::test]
    async fn launcher_failure_reclaims_a_unit_before_returning() {
        #[derive(Clone)]
        struct SequenceProbe(Arc<std::sync::Mutex<std::collections::VecDeque<LocalUnitFact>>>);
        impl LocalUnitProbe for SequenceProbe {
            fn inspect(
                &self,
                _unit: &str,
                _paths: &ExecutionPaths,
            ) -> Result<LocalUnitFact, ExecutorError> {
                self.0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .ok_or_else(|| ExecutorError::UnitProbe {
                        unit: "sequence".to_owned(),
                        detail: "probe sequence exhausted".to_owned(),
                    })
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let base = executor(temp.path());
        let unit = base.unit_name(&request.identity);
        let running = LocalUnitFact {
            unit: unit.clone(),
            loaded: true,
            state: LocalUnitState::Running,
            invocation_id: Some("still-running".to_owned()),
            attempt: Some(request.attempt),
            lease_epoch: Some(request.lease_epoch),
            exit_record: None,
        };
        let probe = SequenceProbe(Arc::new(std::sync::Mutex::new(
            [LocalUnitFact::absent(&unit), running].into(),
        )));
        let systemd_run = temp.path().join("fake-systemd-run");
        std::fs::write(&systemd_run, "#!/bin/sh\nexit 23\n").unwrap();
        let systemctl = temp.path().join("fake-systemctl");
        let marker = temp.path().join("stopped");
        std::fs::write(
            &systemctl,
            format!("#!/bin/sh\nprintf '%s' \"$4\" > {}\n", marker.display()),
        )
        .unwrap();
        for program in [&systemd_run, &systemctl] {
            let mut permissions = std::fs::metadata(program).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(program, permissions).unwrap();
        }
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_systemd_run(systemd_run)
            .with_systemctl(systemctl)
            .with_unit_probe(probe);
        let result = executor.execute(request).await;
        assert!(
            matches!(
                &result,
                Err(ExecutorError::LauncherFailed {
                    status: Some(23),
                    ..
                })
            ),
            "unexpected launcher result: {result:?}"
        );
        assert_eq!(std::fs::read_to_string(marker).unwrap(), unit);
    }

    #[test]
    fn systemd_show_interpretation_is_strict_and_carries_attempt_epoch() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let executor = executor(temp.path());
        let paths = executor.paths(&request.identity);
        let unit = executor.unit_name(&request.identity);
        let running = interpret_systemd_unit_show(
            &unit,
            &paths,
            b"LoadState=loaded\nActiveState=active\nInvocationID=abc123\nEnvironment=\"TALLY_POOL=two words\" TALLY_ATTEMPT=2 TALLY_LEASE_EPOCH=9\n",
        )
        .unwrap();
        assert_eq!(running.state, LocalUnitState::Running);
        assert_eq!(running.attempt, Some(2));
        assert_eq!(running.lease_epoch, Some(9));
        assert!(interpret_systemd_unit_show(
            &unit,
            &paths,
            b"LoadState=loaded\nActiveState=active\nInvocationID=abc123\nEnvironment=TALLY_ATTEMPT=2\n",
        )
        .is_err());
        assert!(interpret_systemd_unit_show(
            &unit,
            &paths,
            b"LoadState=not-found\nActiveState=inactive\nInvocationID=\nEnvironment=\nUnexpected=value\n",
        )
        .is_err());
    }

    #[test]
    fn malformed_mismatched_and_missing_exit_records_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("exit.json");
        std::fs::write(&path, b"{").unwrap();
        assert!(read_exit_record(&path, "unit.service").is_err());
        let record = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: "other.service".to_owned(),
            invocation_id: "id".to_owned(),
            attempt: 1,
            lease_epoch: 1,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        write_exit_record(&path, &record).unwrap();
        assert!(read_exit_record(&path, "unit.service").is_err());
        let incomplete = HashMap::from([
            ("INVOCATION_ID", "id".to_owned()),
            ("SERVICE_RESULT", "success".to_owned()),
            ("TALLY_ATTEMPT", "1".to_owned()),
            ("TALLY_LEASE_EPOCH", "1".to_owned()),
        ]);
        assert!(persist_exit_record(&path, "unit.service", &incomplete).is_err());

        for invalid in [
            UnitExitRecord {
                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                unit: "unit.service".to_owned(),
                invocation_id: "id".to_owned(),
                attempt: 1,
                lease_epoch: 1,
                service_result: "invented".to_owned(),
                exit_code: Some("exited".to_owned()),
                exit_status: Some("0".to_owned()),
            },
            UnitExitRecord {
                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                unit: "unit.service".to_owned(),
                invocation_id: "id".to_owned(),
                attempt: 1,
                lease_epoch: 1,
                service_result: "success".to_owned(),
                exit_code: Some("invented".to_owned()),
                exit_status: Some("0".to_owned()),
            },
            UnitExitRecord {
                schema_version: UNIT_EXIT_SCHEMA_VERSION,
                unit: "unit.service".to_owned(),
                invocation_id: "id".to_owned(),
                attempt: 1,
                lease_epoch: 1,
                service_result: "success".to_owned(),
                exit_code: Some("exited".to_owned()),
                exit_status: None,
            },
        ] {
            write_exit_record(&path, &invalid).unwrap();
            assert!(read_exit_record(&path, "unit.service").is_err());
        }

        let realtime_signal = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: "unit.service".to_owned(),
            invocation_id: "id".to_owned(),
            attempt: 1,
            lease_epoch: 1,
            service_result: "signal".to_owned(),
            exit_code: Some("killed".to_owned()),
            exit_status: Some("RTMIN+1".to_owned()),
        };
        write_exit_record(&path, &realtime_signal).unwrap();
        assert_eq!(
            classify_termination(&read_exit_record(&path, "unit.service").unwrap()).unwrap(),
            ExecutionTermination::Signaled {
                code: "killed".to_owned(),
                status: "RTMIN+1".to_owned(),
            }
        );
    }

    #[test]
    fn startup_failure_without_main_process_metadata_is_durable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("exit.json");
        let environment = HashMap::from([
            ("INVOCATION_ID", "id".to_owned()),
            ("SERVICE_RESULT", "resources".to_owned()),
            ("TALLY_ATTEMPT", "1".to_owned()),
            ("TALLY_LEASE_EPOCH", "1".to_owned()),
        ]);
        let record = persist_exit_record(&path, "unit.service", &environment).unwrap();
        assert_eq!(record.exit_code, None);
        assert_eq!(record.exit_status, None);
        assert_eq!(
            classify_termination(&record).unwrap(),
            ExecutionTermination::ServiceFailed {
                service_result: "resources".to_owned(),
                exit_code: None,
                exit_status: None,
            }
        );
        let json = std::fs::read_to_string(path).unwrap();
        assert!(json.contains("\"exitCode\":null"));
        assert!(json.contains("\"exitStatus\":null"));

        let protocol = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: "unit.service".to_owned(),
            invocation_id: "id".to_owned(),
            attempt: 1,
            lease_epoch: 1,
            service_result: "protocol".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        assert!(matches!(
            classify_termination(&protocol).unwrap(),
            ExecutionTermination::ServiceFailed { .. }
        ));
    }

    #[test]
    fn timeout_records_map_to_runtime_exceeded() {
        let record = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: "unit.service".to_owned(),
            invocation_id: "id".to_owned(),
            attempt: 1,
            lease_epoch: 1,
            service_result: "timeout".to_owned(),
            exit_code: Some("killed".to_owned()),
            exit_status: Some("TERM".to_owned()),
        };
        assert_eq!(
            classify_termination(&record).unwrap(),
            ExecutionTermination::RuntimeExceeded
        );
    }

    #[test]
    fn direct_child_fixture() {
        let Ok(pool) = std::env::var("TALLY_POOL") else {
            return;
        };
        match pool.as_str() {
            "fixture-exit127" => {
                println!("fixture-stdout");
                eprintln!("fixture-stderr");
                std::process::exit(127);
            }
            "fixture-timeout" => {
                if std::env::var_os("TALLY_TEST_DESCENDANT").is_some() {
                    std::thread::sleep(Duration::from_secs(3));
                    std::fs::write("descendant-survived", b"escaped").unwrap();
                    return;
                }
                let executable = std::env::current_exe().unwrap();
                let mut descendant = std::process::Command::new(executable)
                    .args([
                        "executor::tests::direct_child_fixture",
                        "--exact",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("TALLY_TEST_DESCENDANT", "1")
                    .spawn()
                    .unwrap();
                println!("fixture-before-timeout");
                std::thread::sleep(Duration::from_secs(30));
                descendant.wait().unwrap();
            }
            "fixture-reclaim" => {
                if std::env::var_os("TALLY_TEST_DESCENDANT").is_some() {
                    std::fs::write("descendant-started", b"started").unwrap();
                    std::thread::sleep(Duration::from_secs(1));
                    std::fs::write("descendant-leaked", b"escaped").unwrap();
                    return;
                }
                let executable = std::env::current_exe().unwrap();
                let mut descendant = std::process::Command::new(executable)
                    .args([
                        "executor::tests::direct_child_fixture",
                        "--exact",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env("TALLY_TEST_DESCENDANT", "1")
                    .spawn()
                    .unwrap();
                std::fs::write("direct-started", b"started").unwrap();
                std::thread::sleep(Duration::from_secs(30));
                descendant.wait().unwrap();
            }
            _ => {}
        }
    }

    fn fixture_request(pool: &str) -> ExecutionRequest {
        let executable = std::env::current_exe().unwrap();
        ExecutionRequest {
            identity: ExecutionIdentity {
                job_id: Uuid::new_v4(),
                task_uuid: None,
            },
            parent: None,
            pools: vec![pool.to_owned()],
            lease_epoch: 1,
            attempt: 1,
            priority: Priority::Low,
            no_enqueue: false,
            argv: vec![
                executable.to_string_lossy().into_owned(),
                "executor::tests::direct_child_fixture".to_owned(),
                "--exact".to_owned(),
                "--nocapture".to_owned(),
                "--test-threads=1".to_owned(),
            ],
            yield_hook: None,
            tally_socket: None,
            environment: BTreeMap::new(),
            cwd: None,
            credentials: BTreeMap::new(),
            limits: UnitLimits {
                cpu_weight: 100,
                memory_max_bytes: 1024 * 1024,
            },
            runtime_max_sec: None,
        }
    }

    #[tokio::test]
    async fn missing_systemd_run_falls_back_once_and_leaf_127_is_not_retried() {
        let temp = tempfile::tempdir().unwrap();
        let request = fixture_request("fixture-exit127");
        let outcome = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_unit_probe(AbsentProbe)
            .with_systemd_run(temp.path().join("missing-systemd-run"))
            .execute(request)
            .await
            .unwrap();
        assert_eq!(outcome.backend, ExecutionBackend::Direct);
        assert_eq!(outcome.termination, ExecutionTermination::Exited(127));
        let mut stdout = String::new();
        File::open(outcome.paths.stdout)
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        assert_eq!(stdout.matches("fixture-stdout").count(), 1);
        let mut stderr = String::new();
        File::open(outcome.paths.stderr)
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert_eq!(stderr.matches("fixture-stderr").count(), 1);
    }

    #[tokio::test]
    async fn durable_daemon_policy_refuses_direct_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let request = fixture_request("fixture-exit127");
        let result = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_unit_probe(AbsentProbe)
            .with_systemd_run(temp.path().join("missing-systemd-run"))
            .require_systemd()
            .execute(request)
            .await;
        assert!(matches!(result, Err(ExecutorError::Spawn { .. })));
        let capture = temp.path().join(CAPTURE_DIRECTORY);
        assert!(capture
            .read_dir()
            .unwrap()
            .all(|entry| entry.unwrap().metadata().unwrap().len() == 0));
    }

    #[tokio::test]
    async fn direct_fallback_times_out_and_refuses_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_unit_probe(AbsentProbe)
            .with_systemd_run(temp.path().join("missing-systemd-run"));
        let mut timeout = fixture_request("fixture-timeout");
        timeout.runtime_max_sec = Some(1);
        timeout.cwd = Some(temp.path().to_owned());
        let outcome = executor.execute(timeout).await.unwrap();
        assert_eq!(outcome.termination, ExecutionTermination::RuntimeExceeded);
        assert_eq!(outcome.record.service_result, "timeout");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!temp.path().join("descendant-survived").exists());

        let mut credentialed = fixture_request("fixture-exit127");
        credentialed
            .credentials
            .insert("secret".to_owned(), PathBuf::from("/run/secret"));
        assert!(matches!(
            executor.execute(credentialed).await,
            Err(ExecutorError::CredentialedFallback)
        ));
    }
}
