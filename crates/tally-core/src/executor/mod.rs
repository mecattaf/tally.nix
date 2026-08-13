#![allow(clippy::disallowed_macros)]
// Executor diagnostics land in the job's captured streams, not on an operator's
// terminal; they keep the stock macros (#315).

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::watch;
pub use uuid::Uuid;

use crate::adapters::AdapterHardening;
use crate::brief::{self, PreparedBrief};
use crate::completion::{
    evaluate_completion, AcceptancePolicy, ExecutionFact, GateManifestSpec, SemanticCompletion,
};
use crate::config::{ExecutionTargetConfig, Priority, SshExecutorConfig};
use crate::evidence::{parse_evidence_specs, run_evidence_gate, GateResult, RunOutcome};
use crate::exec_attestation::{ExecAttestationContext, EXEC_ATTESTATION_LEDGER};
use crate::provenance::TaskRef;
use crate::taskdb::WorkspaceMetadata;

mod captures;
mod launch;
mod lifecycle;
mod probe;
mod remote;
mod types;

#[cfg(test)]
mod tests;

pub use captures::*;
use launch::*;
use lifecycle::*;
pub use probe::*;
pub use remote::*;
pub use types::*;

#[derive(Debug, Error)]
pub enum ExecutorError {
    #[error("execution request is invalid: {0}")]
    InvalidRequest(String),
    #[error("executor I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "capture lock {path} was still held after {waited_ms}ms; refusing to block on it any longer"
    )]
    CaptureLockContended { path: PathBuf, waited_ms: u128 },
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
    /// The one invalid-record shape a forward migration can repair, typed apart
    /// from the rest so a caller classifies it without parsing prose.
    #[error(
        "unit exit record is invalid: record unit {recorded:?} does not match expected unit {expected:?}"
    )]
    ExitRecordUnitMismatch { recorded: String, expected: String },
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
            allow_direct_fallback: false,
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

    /// The local state root this executor reads durable execution facts from.
    ///
    /// Diagnostics name it so an operator repairing state does not have to
    /// rediscover which directory a message is about.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Opt into the compatibility direct-process backend when `systemd-run`
    /// is absent. Direct execution applies neither transient-unit limits nor
    /// adapter hardening, so library consumers must request it explicitly.
    pub fn with_direct_fallback(mut self) -> Self {
        self.allow_direct_fallback = true;
        self
    }

    /// Explicitly require durable systemd ownership. This is the default, and
    /// remains available to make crash-survivable daemon policy conspicuous at
    /// construction sites.
    pub fn require_systemd(mut self) -> Self {
        self.allow_direct_fallback = false;
        self
    }

    pub fn unit_stem(&self, identity: &ExecutionIdentity) -> String {
        identity.unit_stem()
    }

    pub fn unit_name(&self, identity: &ExecutionIdentity) -> String {
        identity.unit_name()
    }

    pub fn paths(&self, identity: &ExecutionIdentity) -> ExecutionPaths {
        let uuid = identity.unit_uuid();
        let capture_stem = identity.capture_stem();
        ExecutionPaths {
            stdout: self
                .state_dir
                .join(CAPTURE_DIRECTORY)
                .join(format!("{capture_stem}.out")),
            stderr: self
                .state_dir
                .join(CAPTURE_DIRECTORY)
                .join(format!("{capture_stem}.adapter.err")),
            failure_stderr: self
                .state_dir
                .join(CAPTURE_DIRECTORY)
                .join(format!("{capture_stem}.err")),
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
                identity.capture_stem()
            )),
            required_gate_ids: Vec::new(),
            acceptance_policy: AcceptancePolicy::Manual,
        })
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
}
