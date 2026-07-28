use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use taskchampion::Uuid;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::watch;

use crate::adapters::AdapterHardening;
use crate::brief::{self, PreparedBrief};
use crate::completion::{
    evaluate_completion, AcceptancePolicy, ExecutionFact, GateManifestSpec, SemanticCompletion,
};
use crate::config::{ExecutionTargetConfig, Priority, SshExecutorConfig};
use crate::evidence::{parse_evidence_specs, run_evidence_gate, GateResult, RunOutcome};
use crate::exec_attestation::{ExecAttestationContext, EXEC_ATTESTATION_LEDGER};
use crate::git_ai::{self, GitAiExecution};
use crate::taskdb::{GhOrigin, WorkspaceMetadata};
use crate::witness::{Authorship, AuthorshipSession};

pub const CAPTURE_DIRECTORY: &str = "capture";
pub const CAPTURE_ARCHIVE_DIRECTORY: &str = "capture/archive";
pub const UNIT_EXIT_DIRECTORY: &str = "unit-exit";
pub const UNIT_EXIT_SCHEMA_VERSION: u32 = 2;
const OPTIONAL_TALLY_ENVIRONMENT: [&str; 12] = [
    "TALLY_TASK_UUID",
    "TALLY_PARENT",
    "TALLY_NO_ENQUEUE",
    "TALLY_CREDENTIALS",
    "TALLY_YIELD_HOOK",
    "TALLY_SOCKET",
    "TALLY_WORKSPACE_REPO",
    "TALLY_WORKSPACE_BASE_REV",
    "TALLY_WORKSPACE_BRANCH",
    "TALLY_WORKSPACE_PATH",
    "TALLY_BRIEF",
    "TALLY_GATE_MANIFEST",
];
const GH_TALLY_ENVIRONMENT: [&str; 11] = [
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
const GH_CONTEXT_DIRECTORY: &str = "github-context";
const LAUNCH_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(60);

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionIdentity {
    pub job_id: Uuid,
    pub task_uuid: Option<Uuid>,
}

impl ExecutionIdentity {
    pub fn unit_uuid(&self) -> &Uuid {
        self.task_uuid.as_ref().unwrap_or(&self.job_id)
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
    pub credentials: BTreeMap<String, PathBuf>,
    pub limits: UnitLimits,
    pub runtime_max_sec: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionPaths {
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub exit_record: PathBuf,
    pub capture_generation: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetainedCapturePaths {
    pub stdout: PathBuf,
    pub stderr: PathBuf,
    pub current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct CaptureGeneration {
    attempt: u32,
    lease_epoch: u64,
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

pub const REMOTE_EXECUTOR_PROTOCOL_VERSION: u32 = 4;
const MAX_REMOTE_REQUEST_BYTES: u64 = 20 * 1024 * 1024;
const MAX_REMOTE_REPLY_BYTES: usize = 48 * 1024 * 1024;
const MAX_REMOTE_STDERR_BYTES: usize = 64 * 1024;
const MAX_REMOTE_CAPTURE_BYTES: u64 = 16 * 1024 * 1024;

/// Wire protocol used only by the fixed `__remote-executor` helper command.
/// Job argv is nested in the JSON request and is never interpolated into the
/// OpenSSH remote command.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case")]
pub enum RemoteExecutorRequest {
    Ensure {
        state_dir: PathBuf,
        request: ExecutionRequest,
        evidence: Vec<String>,
    },
    Adopt {
        state_dir: PathBuf,
        request: ExecutionRequest,
        expected_invocation_id: String,
        evidence: Vec<String>,
    },
    Probe {
        state_dir: PathBuf,
        identity: ExecutionIdentity,
    },
    Reclaim {
        state_dir: PathBuf,
        identity: ExecutionIdentity,
        #[serde(default)]
        expected_invocation_id: Option<String>,
        attempt: u32,
        lease_epoch: u64,
    },
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RemoteCapture {
    pub attempt: u32,
    pub lease_epoch: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RemoteCompletion {
    pub unit: String,
    pub record: UnitExitRecord,
    pub termination: ExecutionTermination,
    pub capture: RemoteCapture,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_gate: Option<GateResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_completion: Option<SemanticCompletion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship: Option<Authorship>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorship_sessions: Option<Vec<AuthorshipSession>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum RemoteExecutorResult {
    Fact(LocalUnitFact),
    Completion(Box<RemoteCompletion>),
    Reclaimed(RemoteCapture),
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum RemoteExecutorReply {
    Ok {
        protocol_version: u32,
        result: Box<RemoteExecutorResult>,
    },
    Error {
        protocol_version: u32,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{detail}")]
pub struct RemoteTransportError {
    pub detail: String,
}

pub trait RemoteTransport: Send + Sync {
    fn call<'a>(
        &'a self,
        config: &'a SshExecutorConfig,
        request: RemoteExecutorRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>>;
}

#[derive(Debug, Clone, Default)]
pub struct SshRemoteTransport;

pub fn build_ssh_argv(config: &SshExecutorConfig) -> Vec<OsString> {
    let option = |name: &str, value: String| -> OsString { format!("{name}={value}").into() };
    vec![
        "-T".into(),
        "-F".into(),
        "/dev/null".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "PasswordAuthentication=no".into(),
        "-o".into(),
        "KbdInteractiveAuthentication=no".into(),
        "-o".into(),
        "PubkeyAuthentication=yes".into(),
        "-o".into(),
        "IdentitiesOnly=yes".into(),
        "-o".into(),
        "IdentityAgent=none".into(),
        "-o".into(),
        "StrictHostKeyChecking=yes".into(),
        "-o".into(),
        option(
            "UserKnownHostsFile",
            config.known_hosts_file.to_string_lossy().into_owned(),
        ),
        "-o".into(),
        "GlobalKnownHostsFile=/dev/null".into(),
        "-o".into(),
        option("ConnectTimeout", config.connect_timeout_sec.to_string()),
        "-o".into(),
        "ConnectionAttempts=1".into(),
        "-o".into(),
        option(
            "ServerAliveInterval",
            config.server_alive_interval_sec.to_string(),
        ),
        "-o".into(),
        option(
            "ServerAliveCountMax",
            config.server_alive_count_max.to_string(),
        ),
        "-o".into(),
        "ClearAllForwardings=yes".into(),
        "-o".into(),
        "ForwardAgent=no".into(),
        "-o".into(),
        "ForwardX11=no".into(),
        "-o".into(),
        "PermitLocalCommand=no".into(),
        "-o".into(),
        "ProxyCommand=none".into(),
        "-o".into(),
        "CanonicalizeHostname=no".into(),
        "-o".into(),
        "LogLevel=ERROR".into(),
        "-i".into(),
        config.identity_file.as_os_str().to_owned(),
        "-p".into(),
        config.port.to_string().into(),
        "--".into(),
        format!("{}@{}", config.user, config.host).into(),
        config.program.as_os_str().to_owned(),
        "__remote-executor".into(),
    ]
}

async fn read_async_bounded<R>(
    mut reader: R,
    limit: usize,
) -> Result<(Vec<u8>, bool), std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut retained = Vec::new();
    let mut overflow = false;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        if read <= remaining {
            retained.extend_from_slice(&buffer[..read]);
        } else {
            retained.extend_from_slice(&buffer[..remaining]);
            overflow = true;
        }
    }
    Ok((retained, overflow))
}

impl RemoteTransport for SshRemoteTransport {
    fn call<'a>(
        &'a self,
        config: &'a SshExecutorConfig,
        request: RemoteExecutorRequest,
    ) -> Pin<Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut encoded =
                serde_json::to_vec(&request).map_err(|error| RemoteTransportError {
                    detail: format!("cannot encode request: {error}"),
                })?;
            encoded.push(b'\n');
            if encoded.len() as u64 > MAX_REMOTE_REQUEST_BYTES {
                return Err(RemoteTransportError {
                    detail: format!(
                        "request exceeds the {MAX_REMOTE_REQUEST_BYTES}-byte protocol limit"
                    ),
                });
            }

            let mut command = Command::new(&config.ssh_program);
            command
                .kill_on_drop(true)
                .env_clear()
                .env("LC_ALL", "C")
                .args(build_ssh_argv(config))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().map_err(|error| RemoteTransportError {
                detail: format!("cannot spawn {}: {error}", config.ssh_program.display()),
            })?;
            let mut stdin = child.stdin.take().ok_or_else(|| RemoteTransportError {
                detail: "OpenSSH stdin pipe is unavailable".to_owned(),
            })?;
            let stdout = child.stdout.take().ok_or_else(|| RemoteTransportError {
                detail: "OpenSSH stdout pipe is unavailable".to_owned(),
            })?;
            let stderr = child.stderr.take().ok_or_else(|| RemoteTransportError {
                detail: "OpenSSH stderr pipe is unavailable".to_owned(),
            })?;
            let write = async move {
                stdin.write_all(&encoded).await?;
                stdin.shutdown().await
            };
            let wait = child.wait();
            let (write_result, status_result, stdout_result, stderr_result) = tokio::join!(
                write,
                wait,
                read_async_bounded(stdout, MAX_REMOTE_REPLY_BYTES),
                read_async_bounded(stderr, MAX_REMOTE_STDERR_BYTES),
            );
            let status = status_result.map_err(|error| RemoteTransportError {
                detail: format!("cannot wait for OpenSSH: {error}"),
            })?;
            let (stdout, stdout_overflow) =
                stdout_result.map_err(|error| RemoteTransportError {
                    detail: format!("cannot read OpenSSH stdout: {error}"),
                })?;
            let (stderr, stderr_overflow) =
                stderr_result.map_err(|error| RemoteTransportError {
                    detail: format!("cannot read OpenSSH stderr: {error}"),
                })?;
            if let Err(error) = write_result {
                return Err(RemoteTransportError {
                    detail: format!("cannot send remote request: {error}"),
                });
            }
            if stdout_overflow {
                return Err(RemoteTransportError {
                    detail: format!("remote reply exceeds {MAX_REMOTE_REPLY_BYTES} bytes"),
                });
            }
            let stderr_text = String::from_utf8_lossy(&stderr).trim().to_owned();
            if !status.success() {
                return Err(RemoteTransportError {
                    detail: format!(
                        "OpenSSH exited with status {:?}: {}{}",
                        status.code(),
                        stderr_text,
                        if stderr_overflow {
                            " (stderr truncated)"
                        } else {
                            ""
                        }
                    ),
                });
            }
            serde_json::from_slice(&stdout).map_err(|error| RemoteTransportError {
                detail: format!(
                    "remote helper returned invalid JSON: {error}; stderr={stderr_text:?}"
                ),
            })
        })
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
    #[error("unknown remote executor {0:?}")]
    UnknownRemoteExecutor(String),
    #[error("remote executor {executor:?} rejected the operation: {detail}")]
    RemoteExecution { executor: String, detail: String },
    #[error("remote executor protocol error for {executor:?}: {detail}")]
    RemoteProtocol { executor: String, detail: String },
    #[error("{0}")]
    GitAiRequired(String),
    #[error("local execution unit {unit} already exists in state {state:?}")]
    ExistingUnit { unit: String, state: LocalUnitState },
    #[error(
        "execution unit {unit} has a durable launch marker for attempt={attempt}, leaseEpoch={lease_epoch} but no unit or exit record; refusing ambiguous replay"
    )]
    IndeterminatePriorLaunch {
        unit: String,
        attempt: u32,
        lease_epoch: u64,
    },
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

struct LaunchingUnitGuard {
    key: Uuid,
    registry: Arc<Mutex<HashMap<Uuid, watch::Receiver<bool>>>>,
    receiver: watch::Receiver<bool>,
    completed: watch::Sender<bool>,
    armed: bool,
}

impl LaunchingUnitGuard {
    fn mark_complete(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.completed.send(true);
        if let Ok(mut registry) = self.registry.lock() {
            if registry
                .get(&self.key)
                .is_some_and(|receiver| receiver.same_channel(&self.receiver))
            {
                registry.remove(&self.key);
            }
        }
        self.armed = false;
    }
}

impl Drop for LaunchingUnitGuard {
    fn drop(&mut self) {
        self.mark_complete();
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
    launching_units: Arc<Mutex<HashMap<Uuid, watch::Receiver<bool>>>>,
    direct_processes: Arc<Mutex<HashMap<Uuid, DirectProcess>>>,
    allow_direct_fallback: bool,
    remote_executors: Arc<BTreeMap<String, ExecutionTargetConfig>>,
    remote_transport: Arc<dyn RemoteTransport>,
    host_id: Option<String>,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Executor")
            .field("state_dir", &self.state_dir)
            .field("systemd_run", &self.systemd_run)
            .field("systemctl", &self.systemctl)
            .field("recorder_program", &self.recorder_program)
            .field(
                "remote_executors",
                &self.remote_executors.keys().collect::<Vec<_>>(),
            )
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
            launching_units: Arc::new(Mutex::new(HashMap::new())),
            direct_processes: Arc::new(Mutex::new(HashMap::new())),
            allow_direct_fallback: true,
            remote_executors: Arc::new(BTreeMap::new()),
            remote_transport: Arc::new(SshRemoteTransport),
            host_id: crate::witness::current_host_id().ok(),
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

    pub fn with_remote_executors(
        mut self,
        executors: BTreeMap<String, ExecutionTargetConfig>,
    ) -> Self {
        self.remote_executors = Arc::new(executors);
        self
    }

    pub fn with_remote_transport(mut self, transport: impl RemoteTransport + 'static) -> Self {
        self.remote_transport = Arc::new(transport);
        self
    }

    pub fn recorder_program(&self) -> &Path {
        &self.recorder_program
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

    pub fn default_gate_manifest_on(
        &self,
        execution_target: Option<&str>,
        identity: &ExecutionIdentity,
        attempt: u32,
    ) -> Result<GateManifestSpec, ExecutorError> {
        let state_dir = execution_target
            .map(|name| self.remote_config(name).map(|config| config.state_dir))
            .transpose()?
            .unwrap_or_else(|| self.state_dir.clone());
        Ok(GateManifestSpec {
            path: state_dir.join(CAPTURE_DIRECTORY).join(format!(
                "{}.attempt-{attempt}.gates.json",
                identity.unit_uuid()
            )),
            required_gate_ids: Vec::new(),
            acceptance_policy: AcceptancePolicy::Manual,
        })
    }

    pub fn gh_context_path(&self, identity: &ExecutionIdentity) -> PathBuf {
        self.state_dir
            .join(GH_CONTEXT_DIRECTORY)
            .join(format!("{}.json", identity.unit_uuid()))
    }

    pub fn brief_path(&self, hash: &str) -> Result<PathBuf, ExecutorError> {
        brief::content_path(&self.state_dir, hash)
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))
    }

    fn materialize_brief(&self, request: &mut ExecutionRequest) -> Result<(), ExecutorError> {
        let Some(hash) = request.brief_hash.as_deref() else {
            return Ok(());
        };
        if let Some(document) = request.brief_document.take() {
            let prepared = PreparedBrief::from_value(document)
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
            if prepared.hash() != hash {
                return Err(ExecutorError::InvalidRequest(format!(
                    "execution brief hashes to {}, expected {hash}",
                    prepared.hash()
                )));
            }
            create_private_directory(&self.state_dir)?;
            request.brief_path = Some(
                brief::store(&self.state_dir, &prepared)
                    .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?,
            );
        } else {
            let path = request.brief_path.as_ref().ok_or_else(|| {
                ExecutorError::InvalidRequest(
                    "briefHash requires briefPath or briefDocument".to_owned(),
                )
            })?;
            brief::read_verified(path, hash)
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        }
        Ok(())
    }

    fn embed_brief_for_remote(&self, request: &mut ExecutionRequest) -> Result<(), ExecutorError> {
        let Some(hash) = request.brief_hash.as_deref() else {
            return Ok(());
        };
        let path = request.brief_path.as_ref().ok_or_else(|| {
            ExecutorError::InvalidRequest("remote briefHash requires briefPath".to_owned())
        })?;
        let prepared = brief::read_verified(path, hash)
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        request.brief_document = Some(prepared.document().clone());
        request.brief_path = None;
        Ok(())
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

    pub fn retained_capture_paths(
        &self,
        identity: &ExecutionIdentity,
        attempt: u32,
        lease_epoch: u64,
    ) -> Result<Option<RetainedCapturePaths>, ExecutorError> {
        if self.capture_generation_matches(identity, attempt, lease_epoch)? {
            let paths = self.paths(identity);
            return Ok(Some(RetainedCapturePaths {
                stdout: paths.stdout,
                stderr: paths.stderr,
                current: true,
            }));
        }
        let paths = self.archived_capture_paths(identity, attempt, lease_epoch);
        if paths.stdout.exists() || paths.stderr.exists() {
            Ok(Some(paths))
        } else {
            Ok(None)
        }
    }

    fn archived_capture_paths(
        &self,
        identity: &ExecutionIdentity,
        attempt: u32,
        lease_epoch: u64,
    ) -> RetainedCapturePaths {
        let directory = self
            .state_dir
            .join(CAPTURE_ARCHIVE_DIRECTORY)
            .join(identity.unit_uuid().to_string());
        let stem = format!("attempt-{attempt:010}-epoch-{lease_epoch:020}");
        RetainedCapturePaths {
            stdout: directory.join(format!("{stem}.out")),
            stderr: directory.join(format!("{stem}.err")),
            current: false,
        }
    }

    fn archive_current_capture(&self, identity: &ExecutionIdentity) -> Result<(), ExecutorError> {
        let current = self.paths(identity);
        for path in [
            &current.stdout,
            &current.stderr,
            &current.capture_generation,
        ] {
            let metadata = match std::fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(source) => return Err(io_error(path, source)),
            };
            if !metadata.file_type().is_file() || metadata.nlink() != 1 {
                // A partial or replaced capture set has no trustworthy generation.
                // prepare_paths will atomically replace it without following links.
                return Ok(());
            }
        }
        let Some(generation) = read_capture_generation(&current.capture_generation)? else {
            return Ok(());
        };
        let archived =
            self.archived_capture_paths(identity, generation.attempt, generation.lease_epoch);
        let archive_directory = archived
            .stdout
            .parent()
            .expect("archive capture paths always have a parent");
        create_private_directory(archive_directory)?;
        for (source, destination) in [
            (&current.stdout, &archived.stdout),
            (&current.stderr, &archived.stderr),
        ] {
            match std::fs::symlink_metadata(destination) {
                Ok(metadata) => {
                    if !metadata.file_type().is_file()
                        || metadata.nlink() != 1
                        || metadata.permissions().mode() & 0o077 != 0
                    {
                        return Err(ExecutorError::InvalidRequest(format!(
                            "attempt capture archive {} is not a private regular file",
                            destination.display()
                        )));
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    copy_private_file_exclusive(source, destination)?;
                }
                Err(source_error) => return Err(io_error(destination, source_error)),
            }
        }
        sync_directory(archive_directory)?;
        std::fs::remove_file(&current.capture_generation)
            .map_err(|source| io_error(&current.capture_generation, source))?;
        sync_directory(
            current
                .capture_generation
                .parent()
                .expect("capture generation always has a parent"),
        )?;
        for source in [&current.stdout, &current.stderr] {
            match std::fs::remove_file(source) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(source_error) => return Err(io_error(source, source_error)),
            }
        }
        sync_directory(
            current
                .stdout
                .parent()
                .expect("capture stream always has a parent"),
        )?;
        Ok(())
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

    fn remote_config(&self, name: &str) -> Result<SshExecutorConfig, ExecutorError> {
        self.remote_executors
            .get(name)
            .map(ExecutionTargetConfig::ssh)
            .cloned()
            .ok_or_else(|| ExecutorError::UnknownRemoteExecutor(name.to_owned()))
    }

    async fn call_remote(
        &self,
        name: &str,
        request: RemoteExecutorRequest,
    ) -> Result<RemoteExecutorResult, ExecutorError> {
        let config = self.remote_config(name)?;
        let encoded_len = serde_json::to_vec(&request)
            .map_err(|error| ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: format!("cannot encode request: {error}"),
            })?
            .len()
            .saturating_add(1);
        if encoded_len as u64 > MAX_REMOTE_REQUEST_BYTES {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: format!(
                    "request exceeds the {MAX_REMOTE_REQUEST_BYTES}-byte protocol limit"
                ),
            });
        }
        let mut reported_loss = false;
        loop {
            match self.remote_transport.call(&config, request.clone()).await {
                Ok(RemoteExecutorReply::Ok {
                    protocol_version,
                    result,
                }) => {
                    if protocol_version != REMOTE_EXECUTOR_PROTOCOL_VERSION {
                        return Err(ExecutorError::RemoteProtocol {
                            executor: name.to_owned(),
                            detail: format!(
                                "helper protocol version {protocol_version}, expected {REMOTE_EXECUTOR_PROTOCOL_VERSION}"
                            ),
                        });
                    }
                    if reported_loss {
                        eprintln!("tally: remote executor {name:?} is reachable again");
                    }
                    return Ok(*result);
                }
                Ok(RemoteExecutorReply::Error {
                    protocol_version,
                    message,
                }) => {
                    if protocol_version != REMOTE_EXECUTOR_PROTOCOL_VERSION {
                        return Err(ExecutorError::RemoteProtocol {
                            executor: name.to_owned(),
                            detail: format!(
                                "helper error protocol version {protocol_version}, expected {REMOTE_EXECUTOR_PROTOCOL_VERSION}"
                            ),
                        });
                    }
                    if message.starts_with("git-ai-") {
                        return Err(ExecutorError::GitAiRequired(message));
                    }
                    return Err(ExecutorError::RemoteExecution {
                        executor: name.to_owned(),
                        detail: message,
                    });
                }
                Err(error) => {
                    if !reported_loss {
                        eprintln!(
                            "tally: remote executor {name:?} transport is unavailable; retaining leases and retrying: {error}"
                        );
                        reported_loss = true;
                    }
                    tokio::time::sleep(Duration::from_millis(config.retry_interval_ms)).await;
                }
            }
        }
    }

    fn materialize_remote_capture(
        &self,
        identity: &ExecutionIdentity,
        expected_attempt: u32,
        expected_lease_epoch: u64,
        capture: &RemoteCapture,
    ) -> Result<bool, ExecutorError> {
        if capture.attempt != expected_attempt || capture.lease_epoch != expected_lease_epoch {
            return Err(ExecutorError::InvalidRequest(format!(
                "remote capture generation attempt={} leaseEpoch={} does not match expected attempt={expected_attempt} leaseEpoch={expected_lease_epoch}",
                capture.attempt, capture.lease_epoch
            )));
        }
        if let Some(error) = &capture.error {
            eprintln!(
                "tally: remote capture for {} is unavailable: {error}",
                identity.unit_uuid()
            );
            return Ok(false);
        }
        let (Some(stdout), Some(stderr)) = (
            capture.stdout_base64.as_deref(),
            capture.stderr_base64.as_deref(),
        ) else {
            return Err(ExecutorError::InvalidRequest(
                "remote capture omitted data without an error".to_owned(),
            ));
        };
        let stdout = decode_base64(stdout)?;
        let stderr = decode_base64(stderr)?;
        if !self.capture_generation_matches(identity, expected_attempt, expected_lease_epoch)? {
            self.archive_current_capture(identity)?;
        }
        let paths = self.paths(identity);
        replace_private_file(&paths.stdout, &stdout)?;
        replace_private_file(&paths.stderr, &stderr)?;
        write_capture_generation(
            &paths.capture_generation,
            CaptureGeneration {
                attempt: expected_attempt,
                lease_epoch: expected_lease_epoch,
            },
        )?;
        Ok(true)
    }

    fn materialize_remote_completion(
        &self,
        executor_name: &str,
        identity: &ExecutionIdentity,
        expected_invocation_id: Option<&str>,
        expected_attempt: u32,
        expected_lease_epoch: u64,
        completion: RemoteCompletion,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let expected_unit = self.unit_name(identity);
        if completion.unit != expected_unit {
            return Err(ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: format!(
                    "helper returned unit {:?}, expected {expected_unit:?}",
                    completion.unit
                ),
            });
        }
        completion
            .record
            .validate(&expected_unit)
            .map_err(|error| ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: error.to_string(),
            })?;
        if let Some(expected) = expected_invocation_id {
            if completion.record.invocation_id != expected {
                return Err(ExecutorError::AdoptedInvocationMismatch {
                    unit: expected_unit,
                    expected: expected.to_owned(),
                    observed: Some(completion.record.invocation_id),
                });
            }
        }
        if completion.record.attempt != expected_attempt
            || completion.record.lease_epoch != expected_lease_epoch
        {
            return Err(ExecutorError::AdoptedGenerationMismatch {
                unit: expected_unit,
                expected_attempt,
                expected_lease_epoch,
                observed_attempt: completion.record.attempt,
                observed_lease_epoch: completion.record.lease_epoch,
            });
        }
        let classified = classify_termination(&completion.record).map_err(|error| {
            ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: error.to_string(),
            }
        })?;
        if completion.termination != classified {
            return Err(ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: format!(
                    "helper termination {:?} does not match durable exit record classification {:?}",
                    completion.termination, classified
                ),
            });
        }
        if matches!(completion.termination, ExecutionTermination::Exited(_))
            != completion.evidence_gate.is_some()
        {
            return Err(ExecutorError::RemoteProtocol {
                executor: executor_name.to_owned(),
                detail: "helper evidence result does not match the terminal state".to_owned(),
            });
        }
        let captures_available = match self.materialize_remote_capture(
            identity,
            expected_attempt,
            expected_lease_epoch,
            &completion.capture,
        ) {
            Ok(available) => available,
            Err(ExecutorError::InvalidRequest(detail)) => {
                return Err(ExecutorError::RemoteProtocol {
                    executor: executor_name.to_owned(),
                    detail,
                });
            }
            Err(error) => {
                eprintln!(
                    "tally: remote execution completed, but its local capture cache could not be written: {error}"
                );
                false
            }
        };
        let paths = self.paths(identity);
        if let Err(error) = write_exit_record(&paths.exit_record, &completion.record) {
            eprintln!(
                "tally: remote execution completed, but its local exit cache could not be written: {error}"
            );
        }
        Ok(ExecutionOutcome {
            unit: completion.unit,
            backend: ExecutionBackend::Remote,
            paths,
            record: completion.record,
            termination: completion.termination,
            evidence_gate: completion.evidence_gate,
            semantic_completion: completion.semantic_completion,
            result_revision: completion.result_revision,
            authorship: completion.authorship,
            authorship_sessions: completion.authorship_sessions,
            host_id: completion.host_id,
            captures_available,
        })
    }

    pub async fn inspect_identity_on(
        &self,
        executor: Option<&str>,
        identity: &ExecutionIdentity,
    ) -> Result<LocalUnitFact, ExecutorError> {
        let Some(name) = executor else {
            return self.inspect_identity_async(identity).await;
        };
        let config = self.remote_config(name)?;
        let result = self
            .call_remote(
                name,
                RemoteExecutorRequest::Probe {
                    state_dir: config.state_dir,
                    identity: identity.clone(),
                },
            )
            .await?;
        let RemoteExecutorResult::Fact(fact) = result else {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: "probe returned a non-fact response".to_owned(),
            });
        };
        let unit = self.unit_name(identity);
        validate_local_unit_fact_shape(&unit, &fact)?;
        Ok(fact)
    }

    pub async fn execute_on(
        &self,
        executor: Option<&str>,
        mut request: ExecutionRequest,
        evidence: Vec<String>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let Some(name) = executor else {
            return self.execute(request).await;
        };
        self.validate_request(&request)?;
        self.embed_brief_for_remote(&mut request)?;
        self.validate_request(&request)?;
        parse_evidence_specs(&evidence)
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        let config = self.remote_config(name)?;
        let identity = request.identity.clone();
        let attempt = request.attempt;
        let lease_epoch = request.lease_epoch;
        let result = self
            .call_remote(
                name,
                RemoteExecutorRequest::Ensure {
                    state_dir: config.state_dir,
                    request,
                    evidence,
                },
            )
            .await?;
        let RemoteExecutorResult::Completion(completion) = result else {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: "ensure returned a non-completion response".to_owned(),
            });
        };
        self.materialize_remote_completion(name, &identity, None, attempt, lease_epoch, *completion)
    }

    pub async fn adopt_on(
        &self,
        executor: Option<&str>,
        request: ExecutionRequest,
        expected_invocation_id: &str,
        evidence: Vec<String>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let Some(name) = executor else {
            return self.adopt(request, expected_invocation_id).await;
        };
        self.validate_request(&request)?;
        parse_evidence_specs(&evidence)
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        let config = self.remote_config(name)?;
        let identity = request.identity.clone();
        let attempt = request.attempt;
        let lease_epoch = request.lease_epoch;
        let result = self
            .call_remote(
                name,
                RemoteExecutorRequest::Adopt {
                    state_dir: config.state_dir,
                    request,
                    expected_invocation_id: expected_invocation_id.to_owned(),
                    evidence,
                },
            )
            .await?;
        let RemoteExecutorResult::Completion(completion) = result else {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: "adopt returned a non-completion response".to_owned(),
            });
        };
        self.materialize_remote_completion(
            name,
            &identity,
            Some(expected_invocation_id),
            attempt,
            lease_epoch,
            *completion,
        )
    }

    pub async fn reclaim_identity_exact_on(
        &self,
        executor: Option<&str>,
        identity: &ExecutionIdentity,
        expected_invocation_id: Option<&str>,
        attempt: u32,
        lease_epoch: u64,
    ) -> Result<(), ExecutorError> {
        let Some(name) = executor else {
            return self
                .reclaim_identity_exact(identity, expected_invocation_id)
                .await;
        };
        let config = self.remote_config(name)?;
        let result = self
            .call_remote(
                name,
                RemoteExecutorRequest::Reclaim {
                    state_dir: config.state_dir,
                    identity: identity.clone(),
                    expected_invocation_id: expected_invocation_id.map(ToOwned::to_owned),
                    attempt,
                    lease_epoch,
                },
            )
            .await?;
        let RemoteExecutorResult::Reclaimed(capture) = result else {
            return Err(ExecutorError::RemoteProtocol {
                executor: name.to_owned(),
                detail: "reclaim returned a non-reclaimed response".to_owned(),
            });
        };
        self.materialize_remote_capture(identity, attempt, lease_epoch, &capture)?;
        Ok(())
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
        let mut launching = None;
        let mut launch_deadline = None;
        let mut attempt = 0_u16;
        loop {
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
            if launch_deadline.is_none() {
                launching = self
                    .launching_units
                    .lock()
                    .map_err(|_| ExecutorError::UnitControl {
                        unit: fact.unit.clone(),
                        detail: "launch registry is poisoned".to_owned(),
                    })?
                    .get(identity.unit_uuid())
                    .cloned();
                if launching.is_some() {
                    launch_deadline = Some(tokio::time::Instant::now() + LAUNCH_VISIBILITY_TIMEOUT);
                }
            }
            if let Some(deadline) = launch_deadline {
                if tokio::time::Instant::now() >= deadline {
                    return Err(ExecutorError::UnitControl {
                        unit: fact.unit,
                        detail: format!(
                            "execution launch did not become reclaimable within {} seconds",
                            LAUNCH_VISIBILITY_TIMEOUT.as_secs()
                        ),
                    });
                }
            } else if attempt == 200 {
                return Err(ExecutorError::UnitControl {
                    unit: fact.unit,
                    detail: "execution reservation is still held without a reclaimable unit"
                        .to_owned(),
                });
            }
            // The reservation is acquired before either backend becomes
            // externally visible. Give that bounded transition time to publish
            // a systemd unit or direct-process registry entry, then reclaim it.
            if let Some(receiver) = launching.as_mut() {
                let launch_completed = tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(5)) => false,
                    changed = receiver.changed() => {
                        changed.is_err() || *receiver.borrow_and_update()
                    }
                };
                if launch_completed {
                    launching = None;
                }
            } else {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            attempt = attempt.saturating_add(1);
        }
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
        self.build_systemd_argv_with_git_ai(request, None)
    }

    fn build_systemd_argv_with_git_ai(
        &self,
        request: &ExecutionRequest,
        git_ai_runtime: Option<&git_ai::PrivateDaemon>,
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
        self.push_hardening_properties(&mut args, request)?;
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
        let gh_context_path = request
            .gh_origin
            .as_ref()
            .filter(|origin| origin.is_current())
            .map(|_| self.gh_context_path(&request.identity));
        let mut environment = execution_environment(request, gh_context_path.as_deref())?;
        if let Some(runtime) = git_ai_runtime {
            environment.extend(runtime.child_environment());
        }
        for (name, value) in environment {
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
        args.extend(self.execution_argv(request));
        Ok(args)
    }

    fn execution_argv(&self, request: &ExecutionRequest) -> Vec<OsString> {
        let Some(attestation) = &request.exec_attestation else {
            return request.argv.iter().map(OsString::from).collect();
        };
        let mut argv = vec![
            self.recorder_program.as_os_str().to_owned(),
            "attest".into(),
            "exec".into(),
            "--task-uuid".into(),
            request.identity.unit_uuid().to_string().into(),
            "--attempt".into(),
            request.attempt.to_string().into(),
            "--lease-epoch".into(),
            request.lease_epoch.to_string().into(),
            "--adapter".into(),
            attestation.adapter.clone().into(),
        ];
        if let Some(executor) = &attestation.executor {
            argv.extend(["--executor".into(), executor.clone().into()]);
        }
        if let Some(payload_hash) = &attestation.payload_hash {
            argv.extend(["--payload-hash".into(), payload_hash.clone().into()]);
        }
        if let Some(brief_hash) = &attestation.brief_hash {
            argv.extend(["--brief-hash".into(), brief_hash.clone().into()]);
        }
        for evidence in &attestation.evidence {
            argv.extend(["--evidence".into(), evidence.clone().into()]);
        }
        argv.extend([
            "--ledger".into(),
            self.state_dir
                .join(EXEC_ATTESTATION_LEDGER)
                .into_os_string(),
            "--".into(),
        ]);
        argv.extend(request.argv.iter().map(OsString::from));
        argv
    }

    fn push_hardening_properties(
        &self,
        args: &mut Vec<OsString>,
        request: &ExecutionRequest,
    ) -> Result<(), ExecutorError> {
        if request.hardening == AdapterHardening::None {
            return Ok(());
        }
        if request.hardening == AdapterHardening::Strict {
            push_pair(args, "--property", "ProtectHome=read-only");
        }
        push_pair(args, "--property", "PrivateTmp=yes");
        if request.hardening == AdapterHardening::Strict {
            push_pair(args, "--property", "ProtectSystem=strict");
            push_pair(args, "--property", "NoNewPrivileges=yes");
            push_pair(
                args,
                "--property",
                "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
            );
        }
        let mut writable = Vec::new();
        if let Some(workspace) = &request.workspace {
            writable.push(workspace.worktree_path.clone());
            if request.git_ai.is_some() {
                writable.extend(git_repository_write_paths(&workspace.worktree_path));
            }
        }
        writable.push(self.state_dir.clone());
        let mut unique_writable = Vec::new();
        for path in writable {
            if !unique_writable.contains(&path) {
                unique_writable.push(path);
            }
        }
        let writable = unique_writable
            .into_iter()
            .map(|path| quote_systemd_exec_word(path.as_os_str()))
            .collect::<Result<Vec<_>, _>>()?
            .join(" ");
        push_pair(args, "--property", format!("ReadWritePaths={writable}"));
        Ok(())
    }

    pub async fn execute(
        &self,
        request: ExecutionRequest,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let (preflight, runtime) = self.prepare_git_ai(&request).await?;
        let result = match self.execute_raw(request.clone(), runtime.as_ref()).await {
            Ok(outcome) => {
                self.finalize_outcome(outcome, &request, preflight.as_ref(), runtime.as_ref())
                    .await
            }
            Err(error) => Err(error),
        };
        if let Some(runtime) = runtime {
            runtime.shutdown().await;
        }
        result
    }

    async fn execute_raw(
        &self,
        mut request: ExecutionRequest,
        git_ai_runtime: Option<&git_ai::PrivateDaemon>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        self.validate_request(&request)?;
        self.materialize_brief(&mut request)?;
        self.validate_request(&request)?;
        let observed = self.inspect_identity_async(&request.identity).await?;
        let absent_without_exit = match observed.state {
            LocalUnitState::Absent => true,
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
                        evidence_gate: None,
                        semantic_completion: None,
                        result_revision: None,
                        authorship: None,
                        authorship_sessions: None,
                        host_id: self.host_id.clone(),
                        captures_available: true,
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
                false
            }
            LocalUnitState::Running | LocalUnitState::InactiveWithoutRecord => {
                return Err(ExecutorError::ExistingUnit {
                    unit: observed.unit,
                    state: observed.state,
                });
            }
        };
        let reservation = self.reserve(&request.identity)?;
        let mut launching = self.register_launch(&request.identity)?;
        // This marker is fsynced before systemd-run can create the unit. If a
        // retry finds the same generation with neither a unit nor an exit
        // record, the previous helper may have launched work that was lost
        // with the worker. Replaying argv would be a possible duplicate, so
        // preserve the coordinator's lease and require explicit recovery.
        if absent_without_exit
            && self.capture_generation_matches(
                &request.identity,
                request.attempt,
                request.lease_epoch,
            )?
        {
            return Err(ExecutorError::IndeterminatePriorLaunch {
                unit: self.unit_name(&request.identity),
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
            });
        }
        let paths = self.prepare_paths(&request.identity)?;
        write_capture_generation(
            &paths.capture_generation,
            CaptureGeneration {
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
            },
        )?;
        self.materialize_gh_context(&request)?;
        let args = self.build_systemd_argv_with_git_ai(&request, git_ai_runtime)?;
        let output = match Command::new(&self.systemd_run).args(&args).output().await {
            Ok(output) => {
                launching.mark_complete();
                output
            }
            Err(source)
                if source.kind() == std::io::ErrorKind::NotFound && self.allow_direct_fallback =>
            {
                return self.execute_direct(request, paths, git_ai_runtime).await;
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
                drop(reservation);
                if let Err(error) = self.reclaim_identity(&request.identity).await {
                    eprintln!("tally: cannot reclaim {unit} after launcher failure: {error}");
                }
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
            evidence_gate: None,
            semantic_completion: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
            host_id: self.host_id.clone(),
            captures_available: true,
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
        let (preflight, runtime) = self.prepare_git_ai(&request).await?;
        let result = match self
            .adopt_raw(request.clone(), expected_invocation_id)
            .await
        {
            Ok(outcome) => {
                self.finalize_outcome(outcome, &request, preflight.as_ref(), runtime.as_ref())
                    .await
            }
            Err(error) => Err(error),
        };
        if let Some(runtime) = runtime {
            runtime.shutdown().await;
        }
        result
    }

    async fn prepare_git_ai(
        &self,
        request: &ExecutionRequest,
    ) -> Result<(Option<git_ai::Preflight>, Option<git_ai::PrivateDaemon>), ExecutorError> {
        let Some(execution) = &request.git_ai else {
            return Ok((None, None));
        };
        let mut preflight = git_ai::preflight(execution)
            .await
            .map_err(ExecutorError::GitAiRequired)?;
        let Some(workspace) = &request.workspace else {
            return Ok((Some(preflight), None));
        };
        if matches!(preflight, git_ai::Preflight::Failed { .. }) {
            return Ok((Some(preflight), None));
        }
        let runtime_key = format!(
            "{}:{}:{}",
            request.identity.unit_uuid(),
            request.attempt,
            request.lease_epoch
        );
        let mut repository_write_paths = git_repository_write_paths(&workspace.worktree_path);
        repository_write_paths.push(workspace.worktree_path.clone());
        match git_ai::start_private_daemon(
            execution,
            &preflight,
            git_ai::PrivateDaemonLaunch {
                state_dir: &self.state_dir,
                runtime_key: &runtime_key,
                worktree: &workspace.worktree_path,
                repository_write_paths: &repository_write_paths,
                systemd_run: &self.systemd_run,
                systemctl: &self.systemctl,
                allow_direct_fallback: self.allow_direct_fallback,
            },
        )
        .await
        {
            Ok(runtime) => Ok((Some(preflight), Some(runtime))),
            Err(reason) => {
                preflight = git_ai::runtime_failure(execution, &preflight, reason)
                    .map_err(ExecutorError::GitAiRequired)?;
                Ok((Some(preflight), None))
            }
        }
    }

    async fn adopt_raw(
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
                        evidence_gate: None,
                        semantic_completion: None,
                        result_revision: None,
                        authorship: None,
                        authorship_sessions: None,
                        host_id: self.host_id.clone(),
                        captures_available: true,
                    });
                }
            }
        }
    }

    async fn finalize_outcome(
        &self,
        mut outcome: ExecutionOutcome,
        request: &ExecutionRequest,
        preflight: Option<&git_ai::Preflight>,
        git_ai_runtime: Option<&git_ai::PrivateDaemon>,
    ) -> Result<ExecutionOutcome, ExecutorError> {
        let Some(spec) = &request.gate_manifest else {
            return Ok(outcome);
        };
        let execution = execution_fact(&outcome.termination);
        let mut completion = evaluate_completion(execution, spec);
        if let (Some(git_ai), Some(preflight)) = (&request.git_ai, preflight) {
            let worktree = request
                .workspace
                .as_ref()
                .map(|workspace| workspace.worktree_path.as_path());
            let binding =
                git_ai::bind(git_ai, preflight, &completion, worktree, git_ai_runtime).await;
            outcome.result_revision = binding.result_revision;
            outcome.authorship = binding.authorship;
            outcome.authorship_sessions = binding.authorship_sessions;
            if let Some(reason) = binding.required_failure {
                completion = evaluate_completion(ExecutionFact::failed(reason), spec);
            }
        }
        outcome.semantic_completion = Some(completion);
        Ok(outcome)
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
        if let Some(git_ai) = &request.git_ai {
            git_ai.validate().map_err(ExecutorError::InvalidRequest)?;
        }
        if let Some(attestation) = &request.exec_attestation {
            attestation
                .validate()
                .map_err(ExecutorError::InvalidRequest)?;
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
        match (
            request.brief_hash.as_deref(),
            request.brief_path.as_deref(),
            request.brief_document.as_ref(),
        ) {
            (None, None, None) => {}
            (Some(hash), Some(path), None) => {
                brief::content_path(&self.state_dir, hash)
                    .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
                if !path.is_absolute() {
                    return Err(ExecutorError::InvalidRequest(
                        "briefPath must be absolute".to_owned(),
                    ));
                }
                validate_systemd_path(path, "brief path")?;
            }
            (Some(hash), None, Some(document)) => {
                let prepared = PreparedBrief::from_value(document.clone())
                    .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
                if prepared.hash() != hash {
                    return Err(ExecutorError::InvalidRequest(format!(
                        "briefDocument hashes to {}, expected {hash}",
                        prepared.hash()
                    )));
                }
            }
            _ => {
                return Err(ExecutorError::InvalidRequest(
                    "briefHash requires exactly one of briefPath or briefDocument".to_owned(),
                ));
            }
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
        if let Some(gate_manifest) = &request.gate_manifest {
            gate_manifest
                .validate()
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        }
        if let Some(workspace) = &request.workspace {
            workspace
                .validate()
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
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
        if let Some(origin) = &request.gh_origin {
            origin
                .validate()
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        }
        Ok(())
    }

    fn materialize_gh_context(
        &self,
        request: &ExecutionRequest,
    ) -> Result<Option<PathBuf>, ExecutorError> {
        let Some(origin) = request
            .gh_origin
            .as_ref()
            .filter(|origin| origin.is_current())
        else {
            return Ok(None);
        };
        let context = origin.context.as_ref().ok_or_else(|| {
            ExecutorError::InvalidRequest("current GitHub origin omitted context".to_owned())
        })?;
        context
            .validate()
            .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
        let path = self.gh_context_path(&request.identity);
        replace_private_file(&path, &serde_json::to_vec(context)?)?;
        Ok(Some(path))
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

    fn register_launch(
        &self,
        identity: &ExecutionIdentity,
    ) -> Result<LaunchingUnitGuard, ExecutorError> {
        let key = *identity.unit_uuid();
        let (completed, receiver) = watch::channel(false);
        let mut registry = self
            .launching_units
            .lock()
            .map_err(|_| ExecutorError::UnitControl {
                unit: self.unit_name(identity),
                detail: "launch registry is poisoned".to_owned(),
            })?;
        if registry.contains_key(&key) {
            return Err(ExecutorError::UnitControl {
                unit: self.unit_name(identity),
                detail: "execution identity is already registered as launching".to_owned(),
            });
        }
        registry.insert(key, receiver.clone());
        drop(registry);
        Ok(LaunchingUnitGuard {
            key,
            registry: self.launching_units.clone(),
            receiver,
            completed,
            armed: true,
        })
    }

    fn prepare_paths(&self, identity: &ExecutionIdentity) -> Result<ExecutionPaths, ExecutorError> {
        self.archive_current_capture(identity)?;
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
        git_ai_runtime: Option<&git_ai::PrivateDaemon>,
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
        let execution_argv = self.execution_argv(&request);
        let mut command = Command::new(&execution_argv[0]);
        command
            .args(&execution_argv[1..])
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
        let gh_context_path = request
            .gh_origin
            .as_ref()
            .filter(|origin| origin.is_current())
            .map(|_| self.gh_context_path(&request.identity));
        let mut environment = execution_environment(&request, gh_context_path.as_deref())?;
        if let Some(runtime) = git_ai_runtime {
            environment.extend(runtime.child_environment());
        }
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|source| ExecutorError::Spawn {
            program: PathBuf::from(&execution_argv[0]),
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
                        program: PathBuf::from(&execution_argv[0]),
                        source,
                    })?,
                    invocation_id,
                ),
                Err(_) => {
                    terminate_direct_process_group(&mut child, child_pid)
                        .await
                        .map_err(|source| ExecutorError::Spawn {
                            program: PathBuf::from(&execution_argv[0]),
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
                    program: PathBuf::from(&execution_argv[0]),
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
            evidence_gate: None,
            semantic_completion: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
            host_id: self.host_id.clone(),
            captures_available: true,
        })
    }
}

fn git_repository_write_paths(worktree: &Path) -> Vec<PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args([
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
            "--git-common-dir",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .collect()
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
    gh_context_path: Option<&Path>,
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
    if let Some(path) = &request.brief_path {
        environment.push(("TALLY_BRIEF".to_owned(), display_path(path)?.to_owned()));
    }
    if let Some(manifest) = &request.gate_manifest {
        environment.push((
            "TALLY_GATE_MANIFEST".to_owned(),
            display_path(&manifest.path)?.to_owned(),
        ));
    }
    if let Some(git_ai) = &request.git_ai {
        environment.push((
            "GIT_AI_CUSTOM_ATTRIBUTES".to_owned(),
            git_ai.attributes_json()?,
        ));
    }
    if let Some(workspace) = &request.workspace {
        environment.extend([
            ("TALLY_WORKSPACE_REPO".to_owned(), workspace.repo.clone()),
            (
                "TALLY_WORKSPACE_BASE_REV".to_owned(),
                workspace.base_rev.clone(),
            ),
            (
                "TALLY_WORKSPACE_BRANCH".to_owned(),
                workspace.branch.clone(),
            ),
            (
                "TALLY_WORKSPACE_PATH".to_owned(),
                workspace.worktree_path.to_string_lossy().into_owned(),
            ),
        ]);
    }
    if let Some(origin) = request
        .gh_origin
        .as_ref()
        .filter(|origin| origin.is_current())
    {
        let item_type = origin.item_type.ok_or_else(|| {
            ExecutorError::InvalidRequest("current GitHub origin omitted itemType".to_owned())
        })?;
        let context_path = gh_context_path.ok_or_else(|| {
            ExecutorError::InvalidRequest("current GitHub origin omitted context path".to_owned())
        })?;
        environment.extend([
            ("TALLY_GH_REPO".to_owned(), origin.repo.clone()),
            ("TALLY_GH_NUMBER".to_owned(), origin.number.to_string()),
            ("TALLY_GH_URL".to_owned(), origin.html_url.clone()),
            ("TALLY_GH_TYPE".to_owned(), item_type.as_str().to_owned()),
            (
                "TALLY_GH_HEAD_SHA".to_owned(),
                origin.head_sha.clone().unwrap_or_default(),
            ),
            ("TALLY_GH_NODE_ID".to_owned(), origin.node_id.clone()),
            (
                "TALLY_GH_TRIGGER_KIND".to_owned(),
                origin.trigger_kind.clone(),
            ),
            (
                "TALLY_GH_TRIGGER_ACTOR".to_owned(),
                origin.trigger_actor.clone(),
            ),
            (
                "TALLY_GH_EVENT_ID".to_owned(),
                origin.event_id.clone().unwrap_or_default(),
            ),
            (
                "TALLY_GH_COMMENT_ID".to_owned(),
                origin.comment_id.clone().unwrap_or_default(),
            ),
            (
                "TALLY_GH_CONTEXT".to_owned(),
                display_path(context_path)?.to_owned(),
            ),
        ]);
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
    if request.workspace.is_none() {
        names.extend(OPTIONAL_TALLY_ENVIRONMENT[6..10].iter().copied());
    }
    if request.brief_path.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[10]);
    }
    if request.gate_manifest.is_none() {
        names.push(OPTIONAL_TALLY_ENVIRONMENT[11]);
    }
    if request.git_ai.is_none() {
        names.push("GIT_AI_CUSTOM_ATTRIBUTES");
    }
    if request
        .gh_origin
        .as_ref()
        .is_none_or(|origin| !origin.is_current())
    {
        names.extend(GH_TALLY_ENVIRONMENT);
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

fn read_capture_generation(path: &Path) -> Result<Option<CaptureGeneration>, ExecutorError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() || metadata.len() > 1024 {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture generation {} is not a bounded regular file",
            path.display()
        )));
    }
    serde_json::from_reader(file).map(Some).map_err(Into::into)
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

fn copy_private_file_exclusive(source: &Path, destination: &Path) -> Result<(), ExecutorError> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)
        .map_err(|error| io_error(source, error))?;
    let metadata = input.metadata().map_err(|error| io_error(source, error))?;
    if !metadata.file_type().is_file() || metadata.nlink() != 1 {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture {} is not a private regular file",
            source.display()
        )));
    }
    let parent = destination.parent().ok_or_else(|| {
        ExecutorError::InvalidRequest("capture archive path has no parent".to_owned())
    })?;
    create_private_directory(parent)?;
    let file_name = destination
        .file_name()
        .and_then(OsStr::to_str)
        .ok_or_else(|| {
            ExecutorError::InvalidRequest("capture archive name is not valid UTF-8".to_owned())
        })?;
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)
            .map_err(|error| io_error(&temporary, error))?;
        std::io::copy(&mut input, &mut output).map_err(|error| io_error(&temporary, error))?;
        output
            .sync_all()
            .map_err(|error| io_error(&temporary, error))?;
        std::fs::hard_link(&temporary, destination)
            .map_err(|error| io_error(destination, error))?;
        std::fs::remove_file(&temporary).map_err(|error| io_error(&temporary, error))?;
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

pub(crate) fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        output.push(ALPHABET[(first >> 2) as usize] as char);
        output.push(ALPHABET[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        if chunk.len() > 1 {
            output.push(ALPHABET[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char);
        } else {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(ALPHABET[(third & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
    }
    output
}

fn decode_base64(value: &str) -> Result<Vec<u8>, ExecutorError> {
    fn digit(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    if !value.len().is_multiple_of(4)
        || value.len() > (MAX_REMOTE_CAPTURE_BYTES as usize).div_ceil(3) * 4
    {
        return Err(ExecutorError::InvalidRequest(
            "remote capture has an invalid base64 length".to_owned(),
        ));
    }
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    for (index, chunk) in value.as_bytes().chunks_exact(4).enumerate() {
        let last = index + 1 == value.len() / 4;
        let a = digit(chunk[0]).ok_or_else(|| {
            ExecutorError::InvalidRequest("remote capture contains invalid base64".to_owned())
        })?;
        let b = digit(chunk[1]).ok_or_else(|| {
            ExecutorError::InvalidRequest("remote capture contains invalid base64".to_owned())
        })?;
        let c = if chunk[2] == b'=' {
            if !last || chunk[3] != b'=' || b & 0x0f != 0 {
                return Err(ExecutorError::InvalidRequest(
                    "remote capture has non-canonical base64 padding".to_owned(),
                ));
            }
            None
        } else {
            Some(digit(chunk[2]).ok_or_else(|| {
                ExecutorError::InvalidRequest("remote capture contains invalid base64".to_owned())
            })?)
        };
        let d = if chunk[3] == b'=' {
            if !last || c.is_some_and(|value| value & 0x03 != 0) {
                return Err(ExecutorError::InvalidRequest(
                    "remote capture has non-canonical base64 padding".to_owned(),
                ));
            }
            None
        } else {
            Some(digit(chunk[3]).ok_or_else(|| {
                ExecutorError::InvalidRequest("remote capture contains invalid base64".to_owned())
            })?)
        };
        output.push((a << 2) | (b >> 4));
        if let Some(c) = c {
            output.push((b << 4) | (c >> 2));
            if let Some(d) = d {
                output.push((c << 6) | d);
            }
        }
    }
    if output.len() as u64 > MAX_REMOTE_CAPTURE_BYTES {
        return Err(ExecutorError::InvalidRequest(
            "remote capture exceeds its decoded byte limit".to_owned(),
        ));
    }
    Ok(output)
}

fn read_remote_capture(path: &Path) -> Result<Vec<u8>, ExecutorError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| io_error(path, source))?;
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_REMOTE_CAPTURE_BYTES {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture {} is not a bounded regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.take(MAX_REMOTE_CAPTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() as u64 > MAX_REMOTE_CAPTURE_BYTES {
        return Err(ExecutorError::InvalidRequest(format!(
            "capture {} exceeds {MAX_REMOTE_CAPTURE_BYTES} bytes",
            path.display()
        )));
    }
    Ok(bytes)
}

fn collect_remote_capture(paths: &ExecutionPaths, attempt: u32, lease_epoch: u64) -> RemoteCapture {
    match (
        read_remote_capture(&paths.stdout),
        read_remote_capture(&paths.stderr),
    ) {
        (Ok(stdout), Ok(stderr)) => RemoteCapture {
            attempt,
            lease_epoch,
            stdout_base64: Some(encode_base64(&stdout)),
            stderr_base64: Some(encode_base64(&stderr)),
            error: None,
        },
        (stdout, stderr) => RemoteCapture {
            attempt,
            lease_epoch,
            stdout_base64: None,
            stderr_base64: None,
            error: Some(format!(
                "stdout: {}; stderr: {}",
                stdout
                    .err()
                    .map_or_else(|| "ok".to_owned(), |error| error.to_string()),
                stderr
                    .err()
                    .map_or_else(|| "ok".to_owned(), |error| error.to_string())
            )),
        },
    }
}

fn remote_completion(
    outcome: ExecutionOutcome,
    evidence: &[String],
) -> Result<RemoteCompletion, ExecutorError> {
    let gate = match &outcome.termination {
        ExecutionTermination::Exited(exit_code) => {
            let spec = parse_evidence_specs(evidence)
                .map_err(|error| ExecutorError::InvalidRequest(error.to_string()))?;
            Some(run_evidence_gate(RunOutcome {
                exit_code: *exit_code,
                wall_clock_seconds: 0.0,
                evidence: &spec,
            }))
        }
        _ => None,
    };
    let capture = collect_remote_capture(
        &outcome.paths,
        outcome.record.attempt,
        outcome.record.lease_epoch,
    );
    Ok(RemoteCompletion {
        unit: outcome.unit,
        record: outcome.record,
        termination: outcome.termination,
        capture,
        evidence_gate: gate,
        semantic_completion: outcome.semantic_completion,
        result_revision: outcome.result_revision,
        authorship: outcome.authorship,
        authorship_sessions: outcome.authorship_sessions,
        host_id: outcome.host_id,
    })
}

fn execution_fact(termination: &ExecutionTermination) -> ExecutionFact {
    match termination {
        ExecutionTermination::Exited(exit_code) => ExecutionFact::exited(*exit_code),
        ExecutionTermination::RuntimeExceeded => {
            ExecutionFact::failed("process exceeded RuntimeMaxSec")
        }
        ExecutionTermination::Signaled { code, status } => {
            ExecutionFact::failed(format!("process ended by {code} {status}"))
        }
        ExecutionTermination::ServiceFailed { service_result, .. } => {
            ExecutionFact::failed(format!("systemd service failed with {service_result}"))
        }
    }
}

fn pin_remote_reclaim(
    fact: &LocalUnitFact,
    expected_invocation_id: Option<&str>,
    expected_attempt: u32,
    expected_lease_epoch: u64,
) -> Result<Option<String>, ExecutorError> {
    if matches!(fact.state, LocalUnitState::Running | LocalUnitState::Exited) {
        let observed_attempt = fact.attempt.expect("validated present fact has an attempt");
        let observed_lease_epoch = fact
            .lease_epoch
            .expect("validated present fact has a lease epoch");
        if observed_attempt != expected_attempt || observed_lease_epoch != expected_lease_epoch {
            return Err(ExecutorError::AdoptedGenerationMismatch {
                unit: fact.unit.clone(),
                expected_attempt,
                expected_lease_epoch,
                observed_attempt,
                observed_lease_epoch,
            });
        }
    }
    if let Some(expected) = expected_invocation_id {
        if fact.state != LocalUnitState::Absent && fact.invocation_id.as_deref() != Some(expected) {
            return Err(ExecutorError::AdoptedInvocationMismatch {
                unit: fact.unit.clone(),
                expected: expected.to_owned(),
                observed: fact.invocation_id.clone(),
            });
        }
        return Ok(Some(expected.to_owned()));
    }
    Ok(fact.invocation_id.clone())
}

async fn ensure_local_execution(
    executor: &Executor,
    request: ExecutionRequest,
) -> Result<ExecutionOutcome, ExecutorError> {
    loop {
        match executor.execute(request.clone()).await {
            Ok(outcome) => return Ok(outcome),
            Err(
                error @ (ExecutorError::AlreadyRunning(_) | ExecutorError::ExistingUnit { .. }),
            ) => {
                let fact = executor.inspect_identity_async(&request.identity).await?;
                match fact.state {
                    LocalUnitState::Absent => {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    LocalUnitState::Running => {
                        if fact.attempt != Some(request.attempt)
                            || fact.lease_epoch != Some(request.lease_epoch)
                        {
                            return Err(ExecutorError::AdoptedGenerationMismatch {
                                unit: fact.unit,
                                expected_attempt: request.attempt,
                                expected_lease_epoch: request.lease_epoch,
                                observed_attempt: fact.attempt.unwrap_or_default(),
                                observed_lease_epoch: fact.lease_epoch.unwrap_or_default(),
                            });
                        }
                        let invocation =
                            fact.invocation_id.ok_or_else(|| ExecutorError::UnitProbe {
                                unit: fact.unit.clone(),
                                detail: "running remote unit has no invocation identity".to_owned(),
                            })?;
                        return executor.adopt(request, &invocation).await;
                    }
                    LocalUnitState::InactiveWithoutRecord => {
                        let invocation =
                            fact.invocation_id.ok_or_else(|| ExecutorError::UnitProbe {
                                unit: fact.unit.clone(),
                                detail: "inactive remote unit has no invocation identity"
                                    .to_owned(),
                            })?;
                        return executor.adopt(request, &invocation).await;
                    }
                    LocalUnitState::Exited
                        if fact.exit_record.as_ref().is_some_and(|record| {
                            record.attempt == request.attempt
                                && record.lease_epoch == request.lease_epoch
                        }) =>
                    {
                        // `execute` consumes a matching durable exit without
                        // launching, so retrying here is idempotent.
                    }
                    LocalUnitState::Exited => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

async fn handle_remote_executor_request(
    request: RemoteExecutorRequest,
) -> Result<RemoteExecutorResult, ExecutorError> {
    let state_dir = match &request {
        RemoteExecutorRequest::Ensure { state_dir, .. }
        | RemoteExecutorRequest::Adopt { state_dir, .. }
        | RemoteExecutorRequest::Probe { state_dir, .. }
        | RemoteExecutorRequest::Reclaim { state_dir, .. } => state_dir.clone(),
    };
    if !state_dir.is_absolute() {
        return Err(ExecutorError::InvalidRequest(
            "remote stateDir must be absolute".to_owned(),
        ));
    }
    validate_systemd_path(&state_dir, "remote stateDir")?;
    let recorder = std::env::current_exe().map_err(|source| ExecutorError::Io {
        path: PathBuf::from("/proc/self/exe"),
        source,
    })?;
    let executor = Executor::new(state_dir, recorder).require_systemd();
    match request {
        RemoteExecutorRequest::Ensure {
            request, evidence, ..
        } => Ok(RemoteExecutorResult::Completion(Box::new(
            remote_completion(ensure_local_execution(&executor, request).await?, &evidence)?,
        ))),
        RemoteExecutorRequest::Adopt {
            request,
            expected_invocation_id,
            evidence,
            ..
        } => Ok(RemoteExecutorResult::Completion(Box::new(
            remote_completion(
                executor.adopt(request, &expected_invocation_id).await?,
                &evidence,
            )?,
        ))),
        RemoteExecutorRequest::Probe { identity, .. } => Ok(RemoteExecutorResult::Fact(
            executor.inspect_identity_async(&identity).await?,
        )),
        RemoteExecutorRequest::Reclaim {
            identity,
            expected_invocation_id,
            attempt,
            lease_epoch,
            ..
        } => {
            if attempt == 0 || lease_epoch == 0 {
                return Err(ExecutorError::InvalidRequest(
                    "remote reclaim generation must be positive".to_owned(),
                ));
            }
            let fact = executor.inspect_identity_async(&identity).await?;
            let pinned_invocation = pin_remote_reclaim(
                &fact,
                expected_invocation_id.as_deref(),
                attempt,
                lease_epoch,
            )?;
            executor
                .reclaim_identity_exact(&identity, pinned_invocation.as_deref())
                .await?;
            Ok(RemoteExecutorResult::Reclaimed(collect_remote_capture(
                &executor.paths(&identity),
                attempt,
                lease_epoch,
            )))
        }
    }
}

/// Serve exactly one bounded remote-executor request over stdin/stdout.
/// Errors are returned as structured protocol replies so the coordinator can
/// fail closed without guessing whether stderr came from OpenSSH or tally.
pub async fn serve_remote_executor_stdio() -> Result<(), ExecutorError> {
    let request = (|| -> Result<RemoteExecutorRequest, ExecutorError> {
        let mut bytes = Vec::new();
        std::io::stdin()
            .take(MAX_REMOTE_REQUEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| ExecutorError::Io {
                path: PathBuf::from("<stdin>"),
                source,
            })?;
        if bytes.len() as u64 > MAX_REMOTE_REQUEST_BYTES {
            return Err(ExecutorError::InvalidRequest(format!(
                "remote request exceeds {MAX_REMOTE_REQUEST_BYTES} bytes"
            )));
        }
        serde_json::from_slice(&bytes).map_err(ExecutorError::from)
    })();
    let reply = match request {
        Ok(request) => match handle_remote_executor_request(request).await {
            Ok(result) => RemoteExecutorReply::Ok {
                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                result: Box::new(result),
            },
            Err(error) => RemoteExecutorReply::Error {
                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                message: error.to_string(),
            },
        },
        Err(error) => RemoteExecutorReply::Error {
            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
            message: error.to_string(),
        },
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &reply)?;
    stdout
        .write_all(b"\n")
        .and_then(|()| stdout.flush())
        .map_err(|source| ExecutorError::Io {
            path: PathBuf::from("<stdout>"),
            source,
        })
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::io::Read;
    use std::os::unix::ffi::OsStrExt;

    use super::*;
    use crate::taskdb::{
        GhContextSnapshot, GhItemState, GhItemType, GH_CONTEXT_SCHEMA_VERSION,
        GH_ORIGIN_SCHEMA_VERSION,
    };

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
            gh_origin: None,
            brief_hash: None,
            brief_path: None,
            brief_document: None,
            cwd: Some(PathBuf::from("/work tree")),
            workspace: None,
            gate_manifest: None,
            git_ai: None,
            exec_attestation: None,
            hardening: AdapterHardening::None,
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

    fn git_ai_execution() -> GitAiExecution {
        GitAiExecution {
            config: crate::config::GitAiConfig {
                enable: true,
                mode: crate::config::GitAiMode::Advisory,
                await_timeout_sec: 60,
                global_await_ok: true,
            },
            attributes: BTreeMap::from([
                ("adapter".to_owned(), "codex".to_owned()),
                ("attempt".to_owned(), "1".to_owned()),
                ("leaseEpoch".to_owned(), "7".to_owned()),
                (
                    "taskUuid".to_owned(),
                    "00000000-0000-4000-8000-000000000002".to_owned(),
                ),
            ]),
            expected_session: None,
            expected_model: None,
        }
    }

    fn gh_origin(item_type: GhItemType) -> GhOrigin {
        GhOrigin {
            schema_version: GH_ORIGIN_SCHEMA_VERSION,
            producer: "github".to_owned(),
            source: "notifications".to_owned(),
            repo: "acme/widgets".to_owned(),
            number: 77,
            html_url: match item_type {
                GhItemType::Issue => "https://github.com/acme/widgets/issues/77",
                GhItemType::PullRequest => "https://github.com/acme/widgets/pull/77",
            }
            .to_owned(),
            item_type: Some(item_type),
            head_sha: (item_type == GhItemType::PullRequest)
                .then(|| "7777777777777777777777777777777777777777".to_owned()),
            node_id: "I_kwDO_origin".to_owned(),
            item_author: "issue-author".to_owned(),
            trigger_actor: "trusted-maintainer".to_owned(),
            self_actor: "tally-bot".to_owned(),
            notification_reason: Some("mention".to_owned()),
            trigger_kind: "assignment".to_owned(),
            event_id: Some("notification-77".to_owned()),
            comment_id: None,
            trigger_timestamp: Some("2026-07-20T12:30:00Z".to_owned()),
            trigger_value: Some("tally-bot".to_owned()),
            context: Some(GhContextSnapshot {
                schema_version: GH_CONTEXT_SCHEMA_VERSION,
                title: "Untrusted title".to_owned(),
                body: "$(touch /tmp/must-not-run); ${SECRET}".to_owned(),
                state: Some(GhItemState::Open),
                head_sha: (item_type == GhItemType::PullRequest)
                    .then(|| "7777777777777777777777777777777777777777".to_owned()),
                labels: vec!["build".to_owned()],
                assignees: vec!["tally-bot".to_owned()],
                triggering_comment: None,
            }),
            actor_exclude: "self".to_owned(),
            allow_self_triggered: false,
            allowed_actors: vec!["trusted-maintainer".to_owned()],
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

    fn ssh_config() -> SshExecutorConfig {
        SshExecutorConfig {
            host: "worker.example".to_owned(),
            user: "tally-worker".to_owned(),
            port: 2222,
            ssh_program: PathBuf::from("/run/current-system/sw/bin/ssh"),
            identity_file: PathBuf::from("/run/credentials/tally-worker-key"),
            known_hosts_file: PathBuf::from("/etc/tally/worker-known-hosts"),
            program: PathBuf::from("/run/current-system/sw/bin/tally"),
            state_dir: PathBuf::from("/var/lib/tally-remote"),
            connect_timeout_sec: 3,
            server_alive_interval_sec: 2,
            server_alive_count_max: 2,
            retry_interval_ms: 10,
        }
    }

    #[derive(Clone)]
    struct ScriptedRemoteTransport {
        calls: Arc<Mutex<Vec<RemoteExecutorRequest>>>,
        replies: Arc<
            Mutex<std::collections::VecDeque<Result<RemoteExecutorReply, RemoteTransportError>>>,
        >,
    }

    impl ScriptedRemoteTransport {
        fn new(
            replies: impl IntoIterator<Item = Result<RemoteExecutorReply, RemoteTransportError>>,
        ) -> Self {
            Self {
                calls: Arc::new(Mutex::new(Vec::new())),
                replies: Arc::new(Mutex::new(replies.into_iter().collect())),
            }
        }
    }

    impl RemoteTransport for ScriptedRemoteTransport {
        fn call<'a>(
            &'a self,
            _config: &'a SshExecutorConfig,
            request: RemoteExecutorRequest,
        ) -> Pin<
            Box<dyn Future<Output = Result<RemoteExecutorReply, RemoteTransportError>> + Send + 'a>,
        > {
            let calls = self.calls.clone();
            let replies = self.replies.clone();
            Box::pin(async move {
                calls.lock().unwrap().push(request);
                replies.lock().unwrap().pop_front().unwrap_or_else(|| {
                    Err(RemoteTransportError {
                        detail: "scripted remote replies exhausted".to_owned(),
                    })
                })
            })
        }
    }

    fn remote_executor(state_dir: &Path, transport: ScriptedRemoteTransport) -> Executor {
        Executor::new(state_dir, "/nix/store/example/bin/tally")
            .with_remote_executors(BTreeMap::from([(
                "worker".to_owned(),
                ExecutionTargetConfig::Ssh(ssh_config()),
            )]))
            .with_remote_transport(transport)
    }

    fn remote_completion(request: &ExecutionRequest, stdout: &[u8]) -> RemoteCompletion {
        let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
        let record = UnitExitRecord {
            schema_version: UNIT_EXIT_SCHEMA_VERSION,
            unit: unit.clone(),
            invocation_id: "remote-invocation".to_owned(),
            attempt: request.attempt,
            lease_epoch: request.lease_epoch,
            service_result: "success".to_owned(),
            exit_code: Some("exited".to_owned()),
            exit_status: Some("0".to_owned()),
        };
        let evidence = parse_evidence_specs(&["exit:0".to_owned()]).unwrap();
        RemoteCompletion {
            unit,
            record,
            termination: ExecutionTermination::Exited(0),
            capture: RemoteCapture {
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
                stdout_base64: Some(encode_base64(stdout)),
                stderr_base64: Some(encode_base64(b"")),
                error: None,
            },
            evidence_gate: Some(run_evidence_gate(RunOutcome {
                exit_code: 0,
                wall_clock_seconds: 1.0,
                evidence: &evidence,
            })),
            semantic_completion: None,
            result_revision: None,
            authorship: None,
            authorship_sessions: None,
            host_id: Some("worker.example".to_owned()),
        }
    }

    #[test]
    fn ssh_transport_is_fixed_and_never_contains_workload_argv() {
        let config = ssh_config();
        let args = strings(&build_ssh_argv(&config));
        assert_eq!(
            &args[args.len() - 4..],
            [
                "--",
                "tally-worker@worker.example",
                "/run/current-system/sw/bin/tally",
                "__remote-executor",
            ]
        );
        for required in [
            "BatchMode=yes",
            "PasswordAuthentication=no",
            "KbdInteractiveAuthentication=no",
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "StrictHostKeyChecking=yes",
            "UserKnownHostsFile=/etc/tally/worker-known-hosts",
            "GlobalKnownHostsFile=/dev/null",
            "ClearAllForwardings=yes",
            "ForwardAgent=no",
            "ForwardX11=no",
            "ProxyCommand=none",
        ] {
            assert!(args.contains(&required.to_owned()), "missing {required}");
        }
        for workload_argument in &request().argv {
            assert!(
                !args.contains(workload_argument),
                "workload argv leaked into the SSH command: {workload_argument:?}"
            );
        }
    }

    #[test]
    fn remote_capture_base64_is_canonical_and_bounded() {
        for bytes in [
            b"".as_slice(),
            b"f".as_slice(),
            b"fo".as_slice(),
            b"foo".as_slice(),
            &[0, 1, 2, 253, 254, 255],
        ] {
            let encoded = encode_base64(bytes);
            assert_eq!(decode_base64(&encoded).unwrap(), bytes);
        }
        for invalid in ["A===", "Zh==", "Zm9=", "Zm=v", "!!!!", "Zg==AAAA"] {
            assert!(decode_base64(invalid).is_err(), "accepted {invalid:?}");
        }
    }

    #[tokio::test]
    async fn transport_loss_retries_the_same_ensure_without_relaunching() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let completion = remote_completion(&request, b"completed remotely\n");
        let transport = ScriptedRemoteTransport::new([
            Err(RemoteTransportError {
                detail: "connection reset after dispatch".to_owned(),
            }),
            Ok(RemoteExecutorReply::Ok {
                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                result: Box::new(RemoteExecutorResult::Completion(Box::new(
                    completion.clone(),
                ))),
            }),
        ]);
        let calls = transport.calls.clone();
        let executor = remote_executor(temp.path(), transport);
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            executor.execute_on(Some("worker"), request.clone(), vec!["exit:0".to_owned()]),
        )
        .await
        .unwrap()
        .unwrap();

        assert_eq!(outcome.backend, ExecutionBackend::Remote);
        assert_eq!(outcome.record, completion.record);
        assert_eq!(
            std::fs::read(outcome.paths.stdout).unwrap(),
            b"completed remotely\n"
        );
        assert!(outcome.evidence_gate.unwrap().passed);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], calls[1]);
        assert!(matches!(calls[0], RemoteExecutorRequest::Ensure { .. }));
    }

    #[tokio::test]
    async fn durable_launch_marker_blocks_replay_after_worker_loss() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let executor = executor(temp.path());
        let paths = executor.paths(&request.identity);
        write_capture_generation(
            &paths.capture_generation,
            CaptureGeneration {
                attempt: request.attempt,
                lease_epoch: request.lease_epoch,
            },
        )
        .unwrap();

        let error = executor.execute(request.clone()).await.unwrap_err();
        assert!(matches!(
            error,
            ExecutorError::IndeterminatePriorLaunch {
                attempt,
                lease_epoch,
                ..
            } if attempt == request.attempt && lease_epoch == request.lease_epoch
        ));
        assert!(!paths.stdout.exists());
        assert!(!paths.stderr.exists());
        assert!(!paths.exit_record.exists());
    }

    #[tokio::test]
    async fn restart_probe_and_adoption_survive_worker_loss() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let completion = remote_completion(&request, b"adopted\n");
        let fact = LocalUnitFact {
            unit: completion.unit.clone(),
            loaded: true,
            state: LocalUnitState::Running,
            invocation_id: Some(completion.record.invocation_id.clone()),
            attempt: Some(request.attempt),
            lease_epoch: Some(request.lease_epoch),
            exit_record: None,
        };
        let transport = ScriptedRemoteTransport::new([
            Ok(RemoteExecutorReply::Ok {
                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                result: Box::new(RemoteExecutorResult::Fact(fact.clone())),
            }),
            Err(RemoteTransportError {
                detail: "worker temporarily offline".to_owned(),
            }),
            Ok(RemoteExecutorReply::Ok {
                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                result: Box::new(RemoteExecutorResult::Completion(Box::new(completion))),
            }),
        ]);
        let calls = transport.calls.clone();
        let executor = remote_executor(temp.path(), transport);

        assert_eq!(
            executor
                .inspect_identity_on(Some("worker"), &request.identity)
                .await
                .unwrap(),
            fact
        );
        let outcome = tokio::time::timeout(
            Duration::from_secs(1),
            executor.adopt_on(
                Some("worker"),
                request,
                "remote-invocation",
                vec!["exit:0".to_owned()],
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(outcome.backend, ExecutionBackend::Remote);

        let calls = calls.lock().unwrap();
        assert!(matches!(calls[0], RemoteExecutorRequest::Probe { .. }));
        assert_eq!(calls[1], calls[2]);
        assert!(matches!(calls[1], RemoteExecutorRequest::Adopt { .. }));
        assert!(
            !calls
                .iter()
                .any(|call| matches!(call, RemoteExecutorRequest::Ensure { .. })),
            "restart adoption must never issue a fresh launch"
        );
    }

    #[tokio::test]
    async fn malformed_remote_completion_is_a_fail_closed_protocol_error() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let mut completion = remote_completion(&request, b"");
        completion.capture.attempt += 1;
        let transport = ScriptedRemoteTransport::new([Ok(RemoteExecutorReply::Ok {
            protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
            result: Box::new(RemoteExecutorResult::Completion(Box::new(completion))),
        })]);
        let error = remote_executor(temp.path(), transport)
            .execute_on(Some("worker"), request, vec!["exit:0".to_owned()])
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ExecutorError::RemoteProtocol { executor, .. } if executor == "worker"
        ));
    }

    #[tokio::test]
    async fn remote_reclaim_retries_the_exact_invocation_and_generation() {
        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let transport = ScriptedRemoteTransport::new([
            Err(RemoteTransportError {
                detail: "worker disappeared during stop".to_owned(),
            }),
            Ok(RemoteExecutorReply::Ok {
                protocol_version: REMOTE_EXECUTOR_PROTOCOL_VERSION,
                result: Box::new(RemoteExecutorResult::Reclaimed(RemoteCapture {
                    attempt: request.attempt,
                    lease_epoch: request.lease_epoch,
                    stdout_base64: Some(String::new()),
                    stderr_base64: Some(String::new()),
                    error: None,
                })),
            }),
        ]);
        let calls = transport.calls.clone();
        let executor = remote_executor(temp.path(), transport);
        tokio::time::timeout(
            Duration::from_secs(1),
            executor.reclaim_identity_exact_on(
                Some("worker"),
                &request.identity,
                Some("remote-invocation"),
                request.attempt,
                request.lease_epoch,
            ),
        )
        .await
        .unwrap()
        .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], calls[1]);
        assert!(matches!(
            &calls[0],
            RemoteExecutorRequest::Reclaim {
                expected_invocation_id: Some(invocation_id),
                attempt: 1,
                lease_epoch: 7,
                ..
            } if invocation_id == "remote-invocation"
        ));
    }

    #[test]
    fn worker_reclaim_pins_observed_generation_before_stopping() {
        let request = request();
        let unit = format!("tally-job-{}.service", request.identity.unit_uuid());
        let mut fact = LocalUnitFact {
            unit,
            loaded: true,
            state: LocalUnitState::Running,
            invocation_id: Some("observed-invocation".to_owned()),
            attempt: Some(request.attempt),
            lease_epoch: Some(request.lease_epoch),
            exit_record: None,
        };
        assert_eq!(
            pin_remote_reclaim(&fact, None, request.attempt, request.lease_epoch).unwrap(),
            Some("observed-invocation".to_owned())
        );
        assert!(matches!(
            pin_remote_reclaim(
                &fact,
                Some("replacement-invocation"),
                request.attempt,
                request.lease_epoch,
            ),
            Err(ExecutorError::AdoptedInvocationMismatch { .. })
        ));
        fact.attempt = Some(request.attempt + 1);
        assert!(matches!(
            pin_remote_reclaim(&fact, None, request.attempt, request.lease_epoch),
            Err(ExecutorError::AdoptedGenerationMismatch { .. })
        ));
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
    fn exec_attestation_wrapper_is_argv_safe_and_preserves_the_exact_child() {
        let mut request = request();
        let child = request.argv.clone();
        request.exec_attestation = Some(ExecAttestationContext {
            adapter: "codex".to_owned(),
            executor: Some("worker-1".to_owned()),
            payload_hash: Some(format!("sha256:{}", "a".repeat(64))),
            brief_hash: Some(format!("sha256:{}", "b".repeat(64))),
            evidence: vec![
                "exit:0".to_owned(),
                "artifact:/work tree/result.json".to_owned(),
            ],
        });
        let args = strings(
            &executor(Path::new("/state tree"))
                .build_systemd_argv(&request)
                .unwrap(),
        );
        let systemd_separator = args.iter().position(|argument| argument == "--").unwrap();
        assert_eq!(
            &args[systemd_separator + 1..systemd_separator + 4],
            ["/nix/store/example/bin/tally", "attest", "exec"]
        );
        for pair in [
            ["--task-uuid", "00000000-0000-4000-8000-000000000002"],
            ["--attempt", "1"],
            ["--lease-epoch", "7"],
            ["--adapter", "codex"],
            ["--executor", "worker-1"],
            [
                "--payload-hash",
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ],
            [
                "--brief-hash",
                "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ],
            ["--ledger", "/state tree/exec-attestations.jsonl"],
        ] {
            assert!(args.windows(2).any(|window| window == pair));
        }
        assert!(args
            .windows(2)
            .any(|window| { window == ["--evidence", "artifact:/work tree/result.json"] }));
        let child_separator = args.iter().rposition(|argument| argument == "--").unwrap();
        assert!(child_separator > systemd_separator);
        assert_eq!(&args[child_separator + 1..], child);
    }

    #[test]
    fn hardening_preset_names_stamp_only_the_normative_property_bundles() {
        let executor = executor(Path::new("/state tree"));
        let mut strict = request();
        strict.hardening = AdapterHardening::Strict;
        strict.workspace = Some(WorkspaceMetadata {
            repo: "acme/widgets".to_owned(),
            base_rev: "origin/main".to_owned(),
            branch: "tally/work".to_owned(),
            worktree_path: PathBuf::from("/work tree"),
        });
        let strict = strings(&executor.build_systemd_argv(&strict).unwrap());
        for property in [
            "ProtectHome=read-only",
            "PrivateTmp=yes",
            "ProtectSystem=strict",
            "NoNewPrivileges=yes",
            "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
            "ReadWritePaths=\"/work tree\" \"/state tree\"",
        ] {
            assert!(
                strict
                    .windows(2)
                    .any(|pair| pair == ["--property", property]),
                "strict bundle omitted {property}"
            );
        }

        let mut workspace = request();
        workspace.hardening = AdapterHardening::Workspace;
        let workspace = strings(&executor.build_systemd_argv(&workspace).unwrap());
        for property in ["PrivateTmp=yes", "ReadWritePaths=\"/state tree\""] {
            assert!(workspace
                .windows(2)
                .any(|pair| pair == ["--property", property]));
        }
        for forbidden in [
            "ProtectHome=",
            "ProtectSystem=",
            "NoNewPrivileges=",
            "RestrictAddressFamilies=",
        ] {
            assert!(!workspace
                .iter()
                .any(|argument| argument.starts_with(forbidden)));
        }

        let none = strings(&executor.build_systemd_argv(&request()).unwrap());
        for forbidden in [
            "ProtectHome=",
            "PrivateTmp=",
            "ProtectSystem=",
            "NoNewPrivileges=",
            "RestrictAddressFamilies=",
            "ReadWritePaths=",
        ] {
            assert!(!none.iter().any(|argument| argument.starts_with(forbidden)));
        }
    }

    #[test]
    fn git_ai_hardening_grants_the_linked_worktree_common_git_directory() {
        let temp = tempfile::tempdir().unwrap();
        let repository = temp.path().join("repository");
        let worktree = temp.path().join("linked-worktree");
        std::fs::create_dir(&repository).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&repository, &["init", "-q"]);
        git(&repository, &["config", "user.name", "Tally Test"]);
        git(
            &repository,
            &["config", "user.email", "tally@example.invalid"],
        );
        std::fs::write(repository.join("file"), "initial\n").unwrap();
        git(&repository, &["add", "file"]);
        git(&repository, &["commit", "-q", "-m", "initial"]);
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "tally-linked-test",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );

        let mut enabled = request();
        enabled.hardening = AdapterHardening::Strict;
        enabled.workspace = Some(WorkspaceMetadata {
            repo: "acme/widgets".to_owned(),
            base_rev: "HEAD".to_owned(),
            branch: "tally-linked-test".to_owned(),
            worktree_path: worktree.clone(),
        });
        enabled.git_ai = Some(git_ai_execution());
        let args = strings(
            &executor(&temp.path().join("state"))
                .build_systemd_argv(&enabled)
                .unwrap(),
        );
        let writable = args
            .windows(2)
            .find(|pair| pair[0] == "--property" && pair[1].starts_with("ReadWritePaths="))
            .unwrap()[1]
            .clone();
        assert!(writable.contains(worktree.to_str().unwrap()));
        assert!(writable.contains(repository.join(".git").to_str().unwrap()));
        assert!(writable.contains(".git/worktrees/linked-worktree"));
    }

    #[test]
    fn gate_manifest_path_is_exported_or_scrubbed_and_defaults_per_target() {
        let local = executor(Path::new("/coordinator-state"));
        let mut declared = request();
        declared.gate_manifest = Some(GateManifestSpec {
            path: PathBuf::from("/work/gates.json"),
            required_gate_ids: Vec::new(),
            acceptance_policy: AcceptancePolicy::Manual,
        });
        let environment = execution_environment(&declared, None).unwrap();
        assert!(environment
            .iter()
            .any(|(name, value)| { name == "TALLY_GATE_MANIFEST" && value == "/work/gates.json" }));
        assert!(!environment_to_unset(&declared).contains(&"TALLY_GATE_MANIFEST"));
        assert!(environment_to_unset(&request()).contains(&"TALLY_GATE_MANIFEST"));

        let local_default = local
            .default_gate_manifest_on(None, &declared.identity, 3)
            .unwrap();
        assert_eq!(
            local_default.path,
            PathBuf::from(format!(
                "/coordinator-state/capture/{}.attempt-3.gates.json",
                declared.identity.unit_uuid()
            ))
        );
        assert!(local_default.required_gate_ids.is_empty());
        assert_eq!(local_default.acceptance_policy, AcceptancePolicy::Manual);

        let remote = local.with_remote_executors(BTreeMap::from([(
            "worker".to_owned(),
            ExecutionTargetConfig::Ssh(ssh_config()),
        )]));
        let remote_default = remote
            .default_gate_manifest_on(Some("worker"), &declared.identity, 4)
            .unwrap();
        assert_eq!(
            remote_default.path,
            PathBuf::from(format!(
                "/var/lib/tally-remote/capture/{}.attempt-4.gates.json",
                declared.identity.unit_uuid()
            ))
        );
    }

    #[test]
    fn execution_environment_preserves_scalar_compatibility_and_encodes_multi_pool_sets() {
        let singleton = request();
        let singleton_environment = execution_environment(&singleton, None).unwrap();
        assert!(singleton_environment
            .iter()
            .any(|(name, value)| name == "TALLY_POOL" && value == "gpu"));

        let mut multi = request();
        multi.pools = vec!["alpha".to_owned(), "zeta".to_owned()];
        let multi_environment = execution_environment(&multi, None).unwrap();
        assert!(multi_environment
            .iter()
            .any(|(name, value)| { name == "TALLY_POOL" && value == r#"["alpha","zeta"]"# }));
    }

    #[test]
    fn git_ai_custom_attributes_are_exact_and_disabled_integration_is_absent() {
        let mut enabled = request();
        enabled.environment.insert(
            "GIT_AI_CUSTOM_ATTRIBUTES".to_owned(),
            r#"{"spoofed":"value"}"#.to_owned(),
        );
        enabled.git_ai = Some(git_ai_execution());
        let environment = execution_environment(&enabled, None).unwrap();
        assert_eq!(
            environment
                .iter()
                .rev()
                .find(|(name, _)| name == "GIT_AI_CUSTOM_ATTRIBUTES")
                .map(|(_, value)| value.as_str()),
            Some(
                r#"{"adapter":"codex","attempt":"1","leaseEpoch":"7","taskUuid":"00000000-0000-4000-8000-000000000002"}"#
            )
        );
        assert!(!environment_to_unset(&enabled).contains(&"GIT_AI_CUSTOM_ATTRIBUTES"));

        let disabled = request();
        assert!(execution_environment(&disabled, None)
            .unwrap()
            .iter()
            .all(|(name, _)| name != "GIT_AI_CUSTOM_ATTRIBUTES"));
        assert!(environment_to_unset(&disabled).contains(&"GIT_AI_CUSTOM_ATTRIBUTES"));
        assert!(serde_json::to_value(&disabled)
            .unwrap()
            .get("gitAi")
            .is_none());
    }

    #[test]
    fn private_git_ai_runtime_routes_only_the_job_to_its_control_and_trace_sockets() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let executor = executor(&state_dir);
        let mut enabled = request();
        enabled.git_ai = Some(git_ai_execution());
        let runtime = git_ai::private_daemon_paths(
            Path::new("/opt/dotfiles/bin/git-ai"),
            "1.6.17",
            &state_dir,
            "task-53:1:7",
            Path::new("/run/current-system/sw/bin/systemctl"),
        )
        .unwrap();
        let expected = runtime
            .child_environment()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let args = strings(
            &executor
                .build_systemd_argv_with_git_ai(&enabled, Some(&runtime))
                .unwrap(),
        );
        for (name, value) in expected {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "--setenv" && pair[1] == format!("{name}={value}")),
                "job unit omitted {name}"
            );
        }
        std::mem::forget(runtime);
    }

    #[test]
    fn transported_brief_materializes_privately_and_provisions_exact_path() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("remote-state");
        let executor = executor(&state_dir);
        let document = serde_json::json!({
            "mission": "execute remotely",
            "acceptance": ["TALLY_BRIEF is durable"]
        });
        let prepared = PreparedBrief::from_value(document.clone()).unwrap();
        let mut request = request();
        request.brief_hash = Some(prepared.hash().to_owned());
        request.brief_document = Some(document);

        executor.materialize_brief(&mut request).unwrap();
        assert!(request.brief_document.is_none());
        let path = request.brief_path.as_ref().unwrap();
        assert_eq!(
            path,
            &brief::content_path(&state_dir, prepared.hash()).unwrap()
        );
        assert_eq!(
            brief::read_verified(path, prepared.hash()).unwrap(),
            prepared
        );
        assert_eq!(
            std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let environment = execution_environment(&request, None).unwrap();
        assert!(environment
            .iter()
            .any(|(name, value)| name == "TALLY_BRIEF" && value == &path.to_string_lossy()));
    }

    #[test]
    fn github_origin_materializes_private_context_and_exact_identity_environment() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("state");
        let executor = executor(&state_dir);
        let mut request = request();
        request.gh_origin = Some(gh_origin(GhItemType::Issue));
        let original_argv = request.argv.clone();

        let context_path = executor.materialize_gh_context(&request).unwrap().unwrap();
        let environment = execution_environment(&request, Some(&context_path)).unwrap();
        let github = environment
            .iter()
            .filter(|(name, _)| name.starts_with("TALLY_GH_"))
            .cloned()
            .collect::<BTreeMap<_, _>>();
        assert_eq!(github.len(), GH_TALLY_ENVIRONMENT.len());
        assert_eq!(github["TALLY_GH_REPO"], "acme/widgets");
        assert_eq!(github["TALLY_GH_NUMBER"], "77");
        assert_eq!(
            github["TALLY_GH_URL"],
            "https://github.com/acme/widgets/issues/77"
        );
        assert_eq!(github["TALLY_GH_TYPE"], "issue");
        assert_eq!(github["TALLY_GH_HEAD_SHA"], "");
        assert_eq!(github["TALLY_GH_NODE_ID"], "I_kwDO_origin");
        assert_eq!(github["TALLY_GH_TRIGGER_KIND"], "assignment");
        assert_eq!(github["TALLY_GH_TRIGGER_ACTOR"], "trusted-maintainer");
        assert_eq!(github["TALLY_GH_EVENT_ID"], "notification-77");
        assert_eq!(github["TALLY_GH_COMMENT_ID"], "");
        assert_eq!(github["TALLY_GH_CONTEXT"], context_path.to_string_lossy());
        assert_eq!(request.argv, original_argv);
        assert!(github
            .values()
            .all(|value| !value.contains("touch /tmp/must-not-run")));

        let context: GhContextSnapshot =
            serde_json::from_slice(&std::fs::read(&context_path).unwrap()).unwrap();
        assert_eq!(context.schema_version, GH_CONTEXT_SCHEMA_VERSION);
        assert_eq!(context.body, "$(touch /tmp/must-not-run); ${SECRET}");
        assert_eq!(
            std::fs::metadata(&context_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(context_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );

        let mut pull_request = request;
        pull_request.gh_origin = Some(gh_origin(GhItemType::PullRequest));
        let pull_path = executor.gh_context_path(&pull_request.identity);
        let environment = execution_environment(&pull_request, Some(&pull_path)).unwrap();
        assert!(environment.iter().any(|(name, value)| {
            name == "TALLY_GH_HEAD_SHA" && value == "7777777777777777777777777777777777777777"
        }));
    }

    #[test]
    fn jobs_without_github_origin_unset_every_github_identity_variable() {
        let request = request();
        let environment = execution_environment(&request, None).unwrap();
        assert!(environment
            .iter()
            .all(|(name, _)| !name.starts_with("TALLY_GH_")));
        let unset = environment_to_unset(&request);
        for name in GH_TALLY_ENVIRONMENT {
            assert!(unset.contains(&name), "missing unset for {name}");
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
        let unset = args
            .windows(2)
            .find(|pair| pair[0] == "--property" && pair[1].starts_with("UnsetEnvironment="))
            .unwrap();
        let unset_names = unset[1].strip_prefix("UnsetEnvironment=").unwrap();
        for name in OPTIONAL_TALLY_ENVIRONMENT
            .into_iter()
            .chain(["CREDENTIALS_DIRECTORY"])
            .chain(GH_TALLY_ENVIRONMENT)
        {
            assert!(unset_names.split_whitespace().any(|word| word == name));
        }
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
    fn wave_5_red_case_two_attempt_provider_captures_are_distinct_and_queryable() {
        use crate::adapters::{AdapterConfig, AdapterTrace, ScrapeStream, TraceFraming};
        use crate::history::RetentionMetadata;
        use crate::query_v2::{QueryChainHead, QuerySnapshotMetadata};
        use crate::trace::{query_trace, TraceCapability, TraceLane};

        let temp = tempfile::tempdir().unwrap();
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally");
        let request = request();
        let paths = executor.prepare_paths(&request.identity).unwrap();
        std::fs::write(&paths.stdout, b"{\"attempt\":1,\"message\":\"first\"}\n").unwrap();
        write_capture_generation(
            &paths.capture_generation,
            CaptureGeneration {
                attempt: 1,
                lease_epoch: 7,
            },
        )
        .unwrap();

        let second = executor.prepare_paths(&request.identity).unwrap();
        std::fs::write(&second.stdout, b"{\"attempt\":2,\"message\":\"second\"}\n").unwrap();
        write_capture_generation(
            &second.capture_generation,
            CaptureGeneration {
                attempt: 2,
                lease_epoch: 8,
            },
        )
        .unwrap();

        let task_uuid = request.identity.task_uuid.unwrap().to_string();
        let job_id = request.identity.job_id.to_string();
        let lanes = [
            TraceLane {
                task_uuid: task_uuid.clone(),
                job_id: Some(job_id.clone()),
                attempt: 1,
                lease_epoch: 7,
                adapter: "codex".to_owned(),
                session_ref: Some("thread-1".to_owned()),
                running: false,
                remote: false,
            },
            TraceLane {
                task_uuid: task_uuid.clone(),
                job_id: Some(job_id),
                attempt: 2,
                lease_epoch: 8,
                adapter: "codex".to_owned(),
                session_ref: Some("thread-2".to_owned()),
                running: false,
                remote: false,
            },
        ];
        let adapters = BTreeMap::from([(
            "codex".to_owned(),
            AdapterConfig {
                trace: Some(AdapterTrace {
                    stream: ScrapeStream::Stdout,
                    framing: TraceFraming::JsonLines,
                }),
                ..AdapterConfig::default()
            },
        )]);
        let result = query_trace(
            &task_uuid,
            None,
            &lanes,
            &adapters,
            &executor,
            QuerySnapshotMetadata {
                created_at: chrono::Utc::now().to_rfc3339(),
                cursor: None,
                history: RetentionMetadata {
                    complete: true,
                    policy: "unbounded".to_owned(),
                    earliest_cursor: None,
                    latest_cursor: None,
                    truncation_boundary: None,
                    reason: None,
                },
                witness_head: QueryChainHead {
                    seq: 0,
                    hash: "genesis".to_owned(),
                },
            },
        )
        .unwrap();

        assert_eq!(
            result
                .generations
                .iter()
                .map(|generation| (
                    generation.attempt,
                    generation.lease_epoch,
                    generation.capability
                ))
                .collect::<Vec<_>>(),
            vec![
                (1, 7, TraceCapability::Available),
                (2, 8, TraceCapability::Available)
            ]
        );
        assert_eq!(
            result
                .items
                .iter()
                .map(|record| (record.attempt, record.raw.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (1, "{\"attempt\":1,\"message\":\"first\"}"),
                (2, "{\"attempt\":2,\"message\":\"second\"}")
            ]
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
        crate::test_support::install_shell_program(&probe_program, expected_script);

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
        crate::test_support::install_shell_program(&loaded_probe_program, loaded_script);
        let loaded_executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_systemctl(&loaded_probe_program);
        let exited = loaded_executor.inspect_identity(&request.identity).unwrap();
        assert!(exited.loaded);
        assert_eq!(exited.state, LocalUnitState::Exited);
        assert_eq!(exited.exit_record, Some(record));

        let failed_probe_program = temp.path().join("fake-systemctl-failed");
        crate::test_support::install_shell_program(&failed_probe_program, "#!/bin/sh\nexit 23\n");
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
        crate::test_support::install_shell_program(
            &systemctl,
            "#!/bin/sh\nsleep 1\nprintf 'LoadState=not-found\\nActiveState=inactive\\nInvocationID=\\nEnvironment=\\n'\n",
        );
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
        crate::test_support::install_shell_program(&control, script);
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

        crate::test_support::rewrite_shell_program(&control, "#!/bin/sh\nexit 23\n");
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
        crate::test_support::install_shell_program(&systemd_run, "#!/bin/sh\nexit 23\n");
        let systemctl = temp.path().join("fake-systemctl");
        let marker = temp.path().join("stopped");
        crate::test_support::install_shell_program(
            &systemctl,
            format!("#!/bin/sh\nprintf '%s' \"$4\" > {}\n", marker.display()),
        );
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

    #[tokio::test]
    async fn launcher_failure_without_visible_unit_preserves_error_promptly() {
        let temp = tempfile::tempdir().unwrap();
        let systemd_run = temp.path().join("fake-systemd-run");
        crate::test_support::install_shell_program(&systemd_run, "#!/bin/sh\nexit 23\n");
        let executor = Executor::new(temp.path(), "/nix/store/example/bin/tally")
            .with_systemd_run(systemd_run)
            .with_unit_probe(AbsentProbe);

        let result = tokio::time::timeout(Duration::from_millis(100), executor.execute(request()))
            .await
            .expect("launcher failure was masked by reservation reclaim");
        assert!(
            matches!(
                result,
                Err(ExecutorError::LauncherFailed {
                    status: Some(23),
                    ..
                })
            ),
            "unexpected launcher result: {result:?}"
        );
    }

    #[tokio::test]
    async fn reclaim_waits_for_a_registered_launch_to_become_visible() {
        #[derive(Clone)]
        struct VisibilityProbe {
            unit: String,
            visible: PathBuf,
        }

        impl LocalUnitProbe for VisibilityProbe {
            fn inspect(
                &self,
                _unit: &str,
                _paths: &ExecutionPaths,
            ) -> Result<LocalUnitFact, ExecutorError> {
                if self.visible.exists() {
                    Ok(LocalUnitFact {
                        unit: self.unit.clone(),
                        loaded: true,
                        state: LocalUnitState::Running,
                        invocation_id: Some("delayed-launch".to_owned()),
                        attempt: Some(1),
                        lease_epoch: Some(1),
                        exit_record: None,
                    })
                } else {
                    Ok(LocalUnitFact::absent(&self.unit))
                }
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let request = request();
        let base = Executor::new(temp.path(), "/nix/store/example/bin/tally");
        let unit = base.unit_name(&request.identity);
        let started = temp.path().join("launch-started");
        let visible = temp.path().join("unit-visible");
        let systemd_run = temp.path().join("slow-systemd-run");
        crate::test_support::install_shell_program(
            &systemd_run,
            format!(
                "#!/bin/sh\n: > '{}'\nsleep 3\n: > '{}'\nexit 23\n",
                started.display(),
                visible.display()
            ),
        );
        let stopped = temp.path().join("unit-stopped");
        let systemctl = temp.path().join("fake-systemctl");
        crate::test_support::install_shell_program(
            &systemctl,
            format!("#!/bin/sh\nprintf '%s' \"$4\" > '{}'\n", stopped.display()),
        );
        let executor = base
            .with_systemd_run(systemd_run)
            .with_systemctl(systemctl)
            .with_unit_probe(VisibilityProbe {
                unit: unit.clone(),
                visible,
            });
        let running_executor = executor.clone();
        let running_request = request.clone();
        let running = tokio::spawn(async move { running_executor.execute(running_request).await });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fake systemd-run did not start");

        tokio::time::timeout(
            Duration::from_secs(5),
            executor.reclaim_identity(&request.identity),
        )
        .await
        .expect("reclaim did not wait for launch visibility")
        .unwrap();
        assert_eq!(std::fs::read_to_string(stopped).unwrap(), unit);
        let result = running.await.unwrap();
        assert!(
            matches!(
                result,
                Err(ExecutorError::LauncherFailed {
                    status: Some(23),
                    ..
                })
            ),
            "unexpected launcher result: {result:?}"
        );
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
            gh_origin: None,
            brief_hash: None,
            brief_path: None,
            brief_document: None,
            cwd: None,
            workspace: None,
            gate_manifest: None,
            git_ai: None,
            exec_attestation: None,
            hardening: AdapterHardening::None,
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
