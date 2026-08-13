use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tally_client::framing::{ensure_frame_size, read_line_limited, validate_frame_limit};
use tally_client::DEFAULT_MAX_FRAME_BYTES;
pub use tally_client::{RequestFrame, RequestId, WireError, WireErrorCode, WireIoError};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::task::JoinSet;

use crate::adapters::AdapterJobOptions;
use crate::completion::GateManifestSpec;
use crate::config::Priority;
use crate::evidence::parse_evidence_specs;
use crate::provenance::Orchestration;
use crate::taskdb::{
    effective_cwd, gh_trigger_task_uuid, AdmissionOrigin, EnqueueSource, GhOrigin, RelatedTrigger,
    WorkspaceMetadata,
};
use crate::witness::Derivation;

pub const FRAME_CAP_BYTES: usize = DEFAULT_MAX_FRAME_BYTES as usize;
pub const MAX_IN_FLIGHT_REQUESTS: usize = 64;
const PEER_HANGUP_PROBE_INTERVAL: Duration = Duration::from_millis(100);

pub const RPC_METHODS: &[&str] = &[
    "queue.enqueue",
    "queue.continue",
    "queue.retry",
    "queue.cancel",
    "queue.pause",
    "queue.resume",
    "queue.drain",
    "queue.await_job",
    "queue.await_barrier",
    "flow.supersede",
    "lease.acquire",
    "lease.release",
    "lease.status",
    "query.jobs",
    "query.job",
    "query.run",
    "query.lineage",
    "query.status",
    "query.storage",
    "query.log",
    "query.proof",
    "query.trace",
    "query.producers",
    "query.watch",
    "query.render",
    "query.standup",
    "query.pools",
];

pub const INTERNAL_RPC_METHODS: &[&str] = &["__campaign.status", "__producer.runtime-observed"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodClass {
    Client,
    Job,
    Producer,
    Admin,
}

pub fn method_class(method: &str) -> Option<MethodClass> {
    match method {
        "queue.enqueue"
        | "queue.continue"
        | "queue.await_job"
        | "queue.await_barrier"
        | "query.jobs"
        | "query.job"
        | "query.run"
        | "query.lineage"
        | "query.status"
        | "query.storage"
        | "query.log"
        | "query.proof"
        | "query.trace"
        | "query.producers"
        | "query.watch"
        | "query.render"
        | "query.standup"
        | "query.pools"
        | "__campaign.status" => Some(MethodClass::Client),
        "lease.acquire" | "lease.release" | "lease.status" => Some(MethodClass::Job),
        "__producer.runtime-observed" => Some(MethodClass::Producer),
        "queue.pause" | "queue.resume" | "queue.cancel" | "queue.retry" | "queue.drain"
        | "flow.supersede" => Some(MethodClass::Admin),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ResponseOk<'a> {
    id: &'a RequestId,
    result: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ResponseErr<'a> {
    id: &'a RequestId,
    error: WireError,
}

pub trait RpcHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>>;
}

pub async fn serve_connection<H>(stream: UnixStream, handler: H) -> Result<(), WireIoError>
where
    H: RpcHandler + Clone + 'static,
{
    serve_connection_with_limits(stream, handler, DEFAULT_MAX_FRAME_BYTES, None).await
}

pub async fn serve_connection_with_max_frame_bytes<H>(
    stream: UnixStream,
    handler: H,
    max_frame_bytes: u64,
) -> Result<(), WireIoError>
where
    H: RpcHandler + Clone + 'static,
{
    serve_connection_with_limits(stream, handler, max_frame_bytes, None).await
}

pub async fn serve_connection_with_limits<H>(
    stream: UnixStream,
    handler: H,
    max_frame_bytes: u64,
    idle_timeout: Option<Duration>,
) -> Result<(), WireIoError>
where
    H: RpcHandler + Clone + 'static,
{
    validate_frame_limit(max_frame_bytes)?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut reader_open = true;
    let mut requests = JoinSet::new();
    let mut wait_requests = 0;
    while reader_open || !requests.is_empty() {
        if !reader_open {
            if wait_requests == requests.len() {
                tokio::select! {
                    completed = requests.join_next() => {
                        let completed = completed.expect("an in-flight request must be present");
                        write_completed_request(
                            &mut writer,
                            completed,
                            max_frame_bytes,
                            &mut wait_requests,
                        )
                        .await?;
                    }
                    peer_hung_up = peer_write_half_hung_up(writer.as_ref().as_raw_fd()) => {
                        if peer_hung_up? {
                            requests.shutdown().await;
                            return Ok(());
                        }
                    }
                }
            } else {
                let completed = requests
                    .join_next()
                    .await
                    .expect("an in-flight request must be present");
                write_completed_request(
                    &mut writer,
                    completed,
                    max_frame_bytes,
                    &mut wait_requests,
                )
                .await?;
            }
            continue;
        }

        if requests.len() == MAX_IN_FLIGHT_REQUESTS {
            let completed = requests
                .join_next()
                .await
                .expect("an in-flight request must be present");
            write_completed_request(&mut writer, completed, max_frame_bytes, &mut wait_requests)
                .await?;
            continue;
        }

        let read_idle_timeout = if requests.is_empty() {
            idle_timeout
        } else {
            None
        };
        tokio::select! {
            line = read_line_with_idle_timeout(
                &mut reader,
                max_frame_bytes,
                read_idle_timeout,
            ) => {
                let Some(line) = line? else {
                    reader_open = false;
                    continue;
                };
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                let request = match serde_json::from_slice::<RequestFrame>(&line) {
                    Ok(request) => request,
                    Err(error) => {
                        let response = serde_json::json!({
                            "id": Value::Null,
                            "error": WireError::new(WireErrorCode::InvalidFrame, error.to_string()),
                        });
                        write_frame(&mut writer, &response, max_frame_bytes).await?;
                        continue;
                    }
                };
                let request_handler = handler.clone();
                let method = request.method.clone();
                if is_abandonable_wait(&method) {
                    wait_requests += 1;
                }
                requests.spawn_local(async move {
                    let response = async {
                        let encoded = match request_handler.handle(request.clone()).await {
                            Ok(result) => serde_json::to_value(ResponseOk {
                                id: &request.id,
                                result,
                            })?,
                            Err(error) => serde_json::to_value(ResponseErr {
                                id: &request.id,
                                error,
                            })?,
                        };
                        Ok(encoded)
                    }
                    .await;
                    (method, response)
                });
            }
            completed = requests.join_next(), if !requests.is_empty() => {
                let completed = completed.expect("an in-flight request must be present");
                write_completed_request(
                    &mut writer,
                    completed,
                    max_frame_bytes,
                    &mut wait_requests,
                )
                .await?;
            }
        }
    }
    Ok(())
}

async fn read_line_with_idle_timeout<R>(
    reader: &mut R,
    max_frame_bytes: u64,
    idle_timeout: Option<Duration>,
) -> Result<Option<Vec<u8>>, WireIoError>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let read = read_line_limited(reader, max_frame_bytes);
    let Some(idle_timeout) = idle_timeout else {
        return read.await;
    };
    match tokio::time::timeout(idle_timeout, read).await {
        Ok(result) => result,
        Err(_) => Ok(None),
    }
}

async fn write_completed_request(
    writer: &mut OwnedWriteHalf,
    completed: Result<(String, Result<Value, WireIoError>), tokio::task::JoinError>,
    max_frame_bytes: u64,
    wait_requests: &mut usize,
) -> Result<(), WireIoError> {
    let (method, response) =
        completed.map_err(|error| WireIoError::RequestTask(error.to_string()))?;
    let response = response?;
    write_frame(writer, &response, max_frame_bytes).await?;
    if is_abandonable_wait(&method) {
        *wait_requests = wait_requests
            .checked_sub(1)
            .expect("a completed wait must have an in-flight tag");
    }
    Ok(())
}

fn is_abandonable_wait(method: &str) -> bool {
    matches!(method, "queue.await_job" | "queue.await_barrier")
}

async fn peer_write_half_hung_up(fd: RawFd) -> io::Result<bool> {
    loop {
        // SAFETY: `fd` belongs to the live `OwnedWriteHalf` for this connection, and a null
        // buffer is valid for a zero-length send. MSG_NOSIGNAL prevents a closed peer from
        // delivering SIGPIPE to the daemon.
        let sent = unsafe {
            libc::send(
                fd,
                std::ptr::null(),
                0,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if sent >= 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == io::ErrorKind::WouldBlock {
            break;
        }
        match error.raw_os_error() {
            Some(libc::EPIPE | libc::ECONNRESET | libc::ENOTCONN) => return Ok(true),
            _ => return Err(error),
        }
    }
    tokio::time::sleep(PEER_HANGUP_PROBE_INTERVAL).await;
    Ok(false)
}

async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    value: &Value,
    max_frame_bytes: u64,
) -> Result<(), WireIoError> {
    validate_frame_limit(max_frame_bytes)?;
    let mut encoded = serde_json::to_vec(value)?;
    ensure_frame_size(encoded.len(), max_frame_bytes)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    Ok(())
}

pub fn split_invocation(invocation: &str) -> Result<Vec<String>, WireError> {
    let mut output = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut saw_token = false;
    let mut chars = invocation.chars().peekable();
    while let Some(character) = chars.next() {
        if let Some(active_quote) = quote {
            if character == '\\' && active_quote == '"' {
                if let Some(escaped) = chars.next() {
                    current.push(escaped);
                } else {
                    current.push(character);
                }
                continue;
            }
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
            }
            continue;
        }
        if matches!(character, '\'' | '"') {
            quote = Some(character);
            saw_token = true;
        } else if character == '\\' {
            if let Some(escaped) = chars.next() {
                current.push(escaped);
            } else {
                current.push(character);
            }
            saw_token = true;
        } else if character.is_ascii_whitespace() {
            if saw_token {
                output.push(std::mem::take(&mut current));
                saw_token = false;
            }
        } else {
            current.push(character);
            saw_token = true;
        }
    }
    if let Some(active_quote) = quote {
        return Err(WireError::invalid(format!(
            "unterminated {active_quote} quote in invocation: {invocation}"
        )));
    }
    if saw_token {
        output.push(current);
    }
    if output.is_empty() {
        return Err(WireError::invalid(
            "enqueue invocation tokenized to an empty argv",
        ));
    }
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmissionMode {
    Full,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SubmissionOptions {
    pub mode: SubmissionMode,
}

macro_rules! define_enqueue_payload {
    (
        $(
            $(#[$field_attribute:meta])*
            $field:ident: $field_type:ty => $json_name:literal
        ),+ $(,)?
    ) => {
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        pub struct EnqueuePayload {
            $(
                $(#[$field_attribute])*
                pub $field: $field_type,
            )+
        }

        /// JSON field names accepted by the kernel enqueue boundary, in declaration order.
        ///
        /// Defining the struct and this list together makes additions visible to cross-crate
        /// surface-parity tests instead of relying on a second hand-maintained enumeration.
        pub const ENQUEUE_PAYLOAD_FIELDS: &[&str] = &[
            $($json_name,)+
        ];
    };
}

define_enqueue_payload! {
    #[serde(default)]
    invocation: Option<String> => "invocation",
    #[serde(default)]
    argv: Option<Vec<String>> => "argv",
    #[serde(
        default,
        rename = "pool",
        serialize_with = "crate::poolset::serialize_optional",
        deserialize_with = "crate::poolset::deserialize_optional"
    )]
    pools: Option<Vec<String>> => "pool",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    executor: Option<String> => "executor",
    #[serde(default)]
    priority: Option<Priority> => "priority",
    #[serde(default)]
    adapter: Option<String> => "adapter",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<PathBuf> => "cwd",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<WorkspaceMetadata> => "workspace",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    adapter_options: Option<AdapterJobOptions> => "adapterOptions",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    gate_manifest: Option<GateManifestSpec> => "gateManifest",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    brief: Option<Value> => "brief",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    brief_path: Option<PathBuf> => "briefPath",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    resume_from: Option<String> => "resumeFrom",
    #[serde(default)]
    source: Option<EnqueueSource> => "source",
    #[serde(default)]
    dedup_key: Option<String> => "dedupKey",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    submission: Option<SubmissionOptions> => "submission",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    orchestration: Option<Orchestration> => "orchestration",
    #[serde(default)]
    parent: Option<String> => "parent",
    #[serde(default)]
    evidence: Vec<String> => "evidence",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    drv: Option<Derivation> => "drv",
    #[serde(default)]
    evidence_class: Option<Value> => "evidenceClass",
    #[serde(default)]
    manifest_hash: Option<String> => "manifestHash",
    #[serde(default)]
    consumption_estimate: Option<u64> => "consumptionEstimate",
    #[serde(default)]
    runtime_max_sec: Option<u64> => "runtimeMaxSec",
    #[serde(default)]
    no_enqueue: bool => "noEnqueue",
    #[serde(default)]
    credentials: BTreeMap<String, PathBuf> => "credentials",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    origin: Option<AdmissionOrigin> => "origin",
    #[serde(default)]
    caller_job_id: Option<String> => "callerJobId",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    caller_job_token: Option<String> => "callerJobToken",
    #[serde(default, rename = "ghTriggerActor", alias = "ghActor")]
    gh_trigger_actor: Option<String> => "ghTriggerActor",
    #[serde(default)]
    gh_self_actor: Option<String> => "ghSelfActor",
    #[serde(default)]
    gh_origin: Option<GhOrigin> => "ghOrigin",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_uuid: Option<String> => "taskUuid",
    #[serde(default, skip_serializing_if = "Option::is_none")]
    related_trigger: Option<RelatedTrigger> => "relatedTrigger",
    #[serde(default)]
    wait: bool => "wait",
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProducerDefaults {
    pub pools: Vec<String>,
    pub executor: Option<String>,
    pub priority: Priority,
    pub adapter: String,
    pub source: EnqueueSource,
    pub cwd: Option<PathBuf>,
    pub workspace: Option<WorkspaceMetadata>,
    pub adapter_options: AdapterJobOptions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedEnqueue {
    pub argv: Vec<String>,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize",
        deserialize_with = "crate::poolset::deserialize"
    )]
    pub pools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    pub priority: Priority,
    pub adapter: String,
    pub cwd: Option<PathBuf>,
    pub workspace: Option<WorkspaceMetadata>,
    pub adapter_options: AdapterJobOptions,
    pub gate_manifest: Option<GateManifestSpec>,
    pub brief_hash: Option<String>,
    pub resume_from: Option<String>,
    pub source: EnqueueSource,
    pub dedup_key: Option<String>,
    pub orchestration: Option<Orchestration>,
    pub parent: Option<String>,
    pub evidence: Vec<String>,
    pub drv: Option<Derivation>,
    pub evidence_class: Option<Value>,
    pub manifest_hash: Option<String>,
    pub consumption_estimate: Option<u64>,
    pub runtime_max_sec: Option<u64>,
    pub no_enqueue: bool,
    pub credentials: BTreeMap<String, PathBuf>,
    pub origin: AdmissionOrigin,
    pub gh_origin: Option<GhOrigin>,
    pub task_uuid: Option<String>,
    pub related_trigger: Option<RelatedTrigger>,
    pub depth: u32,
    pub wait: bool,
}

impl ResolvedEnqueue {
    /// See [`effective_cwd`]: the admission's working directory, workspace
    /// fallback included. This deliberately does not feed
    /// [`canonical_payload`] -- the payload hash covers the submitted `cwd`,
    /// and the enqueue kernel's dedup arithmetic must not shift because a
    /// render site learned to read the workspace.
    pub fn effective_cwd(&self) -> Option<&Path> {
        effective_cwd(self.cwd.as_deref(), self.workspace.as_ref())
    }
}

// Kernel-side counterpart to tally_flow's NodeSpec canonical field contract.
// Full-mode flow submissions keep cwd and gateManifest absent until NodeSpec
// exposes them; the tally crate's structural parity test pins this exact split.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalPayload<'a> {
    argv: &'a [String],
    #[serde(rename = "pool", serialize_with = "crate::poolset::serialize")]
    pools: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    executor: Option<&'a str>,
    adapter: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<&'a Path>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<&'a WorkspaceMetadata>,
    adapter_options: &'a AdapterJobOptions,
    #[serde(skip_serializing_if = "Option::is_none")]
    gate_manifest: Option<&'a GateManifestSpec>,
    evidence: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    drv: Option<&'a Derivation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    evidence_class: Option<&'a Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest_hash: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_max_sec: Option<u64>,
    no_enqueue: bool,
    credentials: &'a BTreeMap<String, PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    brief_hash: Option<&'a str>,
}

pub fn canonical_payload(resolved: &ResolvedEnqueue) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(&CanonicalPayload {
        argv: &resolved.argv,
        pools: &resolved.pools,
        executor: resolved.executor.as_deref(),
        adapter: &resolved.adapter,
        cwd: resolved.cwd.as_deref(),
        workspace: resolved.workspace.as_ref(),
        adapter_options: &resolved.adapter_options,
        gate_manifest: resolved.gate_manifest.as_ref(),
        evidence: &resolved.evidence,
        drv: resolved.drv.as_ref(),
        evidence_class: resolved.evidence_class.as_ref(),
        manifest_hash: resolved.manifest_hash.as_deref(),
        runtime_max_sec: resolved.runtime_max_sec,
        no_enqueue: resolved.no_enqueue,
        credentials: &resolved.credentials,
        brief_hash: resolved.brief_hash.as_deref(),
    })
}

pub fn canonical_payload_hash(resolved: &ResolvedEnqueue) -> Result<String, serde_json::Error> {
    let bytes = canonical_payload(resolved)?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentInfo {
    pub parent_uuid: String,
    pub depth: u32,
    pub outstanding: u32,
    pub no_enqueue: bool,
    pub terminal: bool,
}

#[derive(Debug, Clone)]
pub struct GuardrailConfig {
    pub depth_cap: u32,
    pub fanout_cap: u32,
    pub require_dedup_key: bool,
}

impl Default for GuardrailConfig {
    fn default() -> Self {
        Self {
            depth_cap: 3,
            fanout_cap: 64,
            require_dedup_key: true,
        }
    }
}

#[derive(Debug, Default)]
pub struct GuardrailState {
    config: GuardrailConfig,
    parents: HashMap<String, ParentInfo>,
}

impl GuardrailState {
    pub fn new(config: GuardrailConfig) -> Result<Self, WireError> {
        if config.depth_cap == 0 || config.fanout_cap == 0 {
            return Err(WireError::invalid(
                "depthCap and fanoutCap must both be positive",
            ));
        }
        Ok(Self {
            config,
            parents: HashMap::new(),
        })
    }

    pub fn register_parent(&mut self, job_id: impl Into<String>, info: ParentInfo) {
        self.parents.insert(job_id.into(), info);
    }

    pub fn parent(&self, job_id: &str) -> Option<&ParentInfo> {
        self.parents.get(job_id)
    }

    pub fn parent_count(&self) -> usize {
        self.parents.len()
    }

    pub fn retire_parent(&mut self, job_id: &str) {
        if self
            .parents
            .get(job_id)
            .is_some_and(|info| info.outstanding == 0)
        {
            self.parents.remove(job_id);
        } else if let Some(info) = self.parents.get_mut(job_id) {
            info.terminal = true;
        }
    }

    pub fn rollback_child_charge(&mut self, job_id: &str) -> Result<(), WireError> {
        let remove = {
            let info = self
                .parents
                .get_mut(job_id)
                .ok_or_else(|| WireError::not_found(format!("unknown parent job {job_id}")))?;
            info.outstanding = info.outstanding.checked_sub(1).ok_or_else(|| {
                WireError::new(
                    WireErrorCode::Internal,
                    format!("parent job {job_id} has no outstanding child charge"),
                )
            })?;
            info.terminal && info.outstanding == 0
        };
        if remove {
            self.parents.remove(job_id);
        }
        Ok(())
    }

    pub fn charge_child(&mut self, job_id: &str) -> Result<(), WireError> {
        let info = self
            .parents
            .get_mut(job_id)
            .ok_or_else(|| WireError::not_found(format!("unknown parent job {job_id}")))?;
        if info.outstanding >= self.config.fanout_cap {
            return Err(WireError::invalid(format!(
                "parent fanout would exceed fanoutCap {}",
                self.config.fanout_cap
            )));
        }
        info.outstanding += 1;
        Ok(())
    }

    pub fn validate_enqueue(
        &mut self,
        mut payload: EnqueuePayload,
        defaults: &ProducerDefaults,
    ) -> Result<ResolvedEnqueue, WireError> {
        let argv = match (payload.invocation.as_deref(), payload.argv) {
            (Some(invocation), None) => split_invocation(invocation)?,
            (None, Some(argv)) if !argv.is_empty() => argv,
            (Some(_), Some(_)) => {
                return Err(WireError::invalid(
                    "enqueue requires invocation XOR argv, not both",
                ));
            }
            _ => {
                return Err(WireError::invalid(
                    "enqueue requires a non-empty invocation XOR argv",
                ));
            }
        };
        let full_submission = payload
            .submission
            .as_ref()
            .is_some_and(|submission| submission.mode == SubmissionMode::Full);
        if full_submission && payload.orchestration.is_some() {
            if payload.cwd.is_some() {
                return Err(WireError::invalid(
                    "full-mode flow submissions require cwd to be absent until NodeSpec exposes it",
                ));
            }
            if payload.gate_manifest.is_some() {
                return Err(WireError::invalid(
                    "full-mode flow submissions require gateManifest to be absent until NodeSpec exposes it",
                ));
            }
        }
        if payload.runtime_max_sec == Some(0) {
            return Err(WireError::invalid(
                "runtimeMaxSec must be positive when set",
            ));
        }
        if let Some(cwd) = &payload.cwd {
            validate_path(cwd, "cwd")?;
        }
        if let Some(workspace) = &payload.workspace {
            workspace
                .validate()
                .map_err(|error| WireError::invalid(error.to_string()))?;
        }
        if let Some(gate_manifest) = &payload.gate_manifest {
            gate_manifest
                .validate()
                .map_err(|error| WireError::invalid(error.to_string()))?;
        }
        if let Some(resume_from) = &payload.resume_from {
            uuid::Uuid::parse_str(resume_from)
                .map_err(|_| WireError::invalid("resumeFrom must be a task UUID"))?;
            if payload.task_uuid.is_some() {
                return Err(WireError::invalid(
                    "resumeFrom and a preassigned taskUuid are mutually exclusive",
                ));
            }
        }
        validate_credentials(&payload.credentials)?;
        let evidence_spec = parse_evidence_specs(&payload.evidence)
            .map_err(|error| WireError::invalid(error.to_string()))?;
        let evidence = evidence_spec.render();

        let source = payload.source.unwrap_or(defaults.source);
        if payload.gh_origin.is_none() {
            payload.gh_origin = payload
                .origin
                .as_ref()
                .and_then(|origin| origin.github.clone());
        }
        let origin = payload.origin.clone().unwrap_or_else(|| {
            payload.gh_origin.as_ref().map_or_else(
                || AdmissionOrigin::direct(source),
                |github| AdmissionOrigin::github(&github.producer, github.clone()),
            )
        });
        if origin.source != source {
            return Err(WireError::invalid(
                "origin source does not match enqueue source",
            ));
        }
        origin
            .validate()
            .map_err(|error| WireError::invalid(error.to_string()))?;
        if origin.github.as_ref() != payload.gh_origin.as_ref() {
            return Err(WireError::invalid(
                "legacy ghOrigin and nested origin github detail disagree",
            ));
        }
        if payload.gh_origin.is_some() && source != EnqueueSource::Gh {
            return Err(WireError::invalid("ghOrigin is valid only for source=gh"));
        }
        if let Some(related) = &payload.related_trigger {
            if source == EnqueueSource::Gh {
                return Err(WireError::invalid(
                    "relatedTrigger is fallback provenance and is invalid for source=gh",
                ));
            }
            related
                .validate()
                .map_err(|error| WireError::invalid(error.to_string()))?;
        }
        if let Some(origin) = &payload.gh_origin {
            origin
                .validate()
                .map_err(|error| WireError::invalid(error.to_string()))?;
            if payload.gh_trigger_actor.as_deref() != Some(origin.trigger_actor.as_str())
                || payload.gh_self_actor.as_deref() != Some(origin.self_actor.as_str())
            {
                return Err(WireError::invalid(
                    "GitHub trigger actor fields do not match the durable ghOrigin",
                ));
            }
        }
        if let Some(task_uuid) = &payload.task_uuid {
            uuid::Uuid::parse_str(task_uuid)
                .map_err(|_| WireError::invalid("taskUuid must be a UUID"))?;
            if let Some(origin) = payload.gh_origin.as_ref() {
                let expected = gh_trigger_task_uuid(origin)
                    .map_err(|error| WireError::invalid(error.to_string()))?
                    .to_string();
                if task_uuid != &expected {
                    return Err(WireError::invalid(
                        "preassigned GitHub taskUuid does not match its trigger identity",
                    ));
                }
            } else if payload.drv.is_none() {
                return Err(WireError::invalid(
                    "taskUuid may be preassigned only by a GitHub trigger or drv seed",
                ));
            }
        }
        let excluded = payload.gh_origin.as_ref().is_some_and(|origin| {
            if origin.schema_version == 0 {
                if origin.actor_exclude == "self" {
                    origin.trigger_actor == origin.self_actor
                } else {
                    origin.trigger_actor == origin.actor_exclude
                }
            } else {
                (!origin.allowed_actors.is_empty()
                    && !origin
                        .allowed_actors
                        .iter()
                        .any(|actor| actor == &origin.trigger_actor))
                    || (origin.trigger_actor == origin.self_actor && !origin.allow_self_triggered)
                    || origin.trigger_actor == origin.actor_exclude
            }
        }) || payload.gh_origin.is_none()
            && payload
                .gh_trigger_actor
                .as_deref()
                .zip(payload.gh_self_actor.as_deref())
                .is_some_and(|(actor, own)| actor == own);
        if source == EnqueueSource::Gh && excluded {
            return Err(WireError::invalid(
                "GitHub trigger actor is filtered by producer policy",
            ));
        }
        let mut pools = payload.pools.unwrap_or_else(|| defaults.pools.clone());
        crate::poolset::canonicalize(&mut pools)
            .map_err(|error| WireError::invalid(error.to_string()))?;
        let mut drv = payload.drv;
        if let Some(drv) = &mut drv {
            drv.canonicalize().map_err(WireError::invalid)?;
            if payload.task_uuid.is_none() {
                return Err(WireError::invalid(
                    "drv enqueue requires the submitted seed taskUuid",
                ));
            }
            let expected_key = format!("drv:{}", drv.drv_path);
            if pools != ["build"]
                || payload.dedup_key.as_deref() != Some(expected_key.as_str())
                || evidence_spec.declared_store_paths() != drv.output_paths()
            {
                return Err(WireError::invalid(
                    "drv enqueue requires pool [\"build\"], dedupKey drv:<drvPath>, and store evidence exactly matching all outputs",
                ));
            }
        }
        let adapter = payload.adapter.unwrap_or_else(|| defaults.adapter.clone());
        if adapter.trim().is_empty() {
            return Err(WireError::invalid("adapter must not be empty"));
        }

        let mut parent = payload.parent;
        let mut depth = 0;
        if let Some(caller_job_id) = payload.caller_job_id {
            let info = self.parents.get_mut(&caller_job_id).ok_or_else(|| {
                WireError::not_found(format!("unknown parent job {caller_job_id}"))
            })?;
            if info.terminal {
                return Err(WireError::not_found(format!(
                    "parent job {caller_job_id} is terminal"
                )));
            }
            if info.no_enqueue {
                return Err(WireError::invalid(format!(
                    "job {caller_job_id} carries the noEnqueue capability"
                )));
            }
            if self.config.require_dedup_key
                && payload
                    .dedup_key
                    .as_ref()
                    .is_none_or(|key| key.trim().is_empty())
            {
                return Err(WireError::invalid(
                    "job-originated enqueue requires dedupKey",
                ));
            }
            depth = info.depth + 1;
            if depth > self.config.depth_cap {
                return Err(WireError::invalid(format!(
                    "enqueue depth {depth} exceeds depthCap {}",
                    self.config.depth_cap
                )));
            }
            if !full_submission && info.outstanding >= self.config.fanout_cap {
                return Err(WireError::invalid(format!(
                    "parent fanout would exceed fanoutCap {}",
                    self.config.fanout_cap
                )));
            }
            parent = Some(info.parent_uuid.clone());
            if !full_submission {
                info.outstanding += 1;
            }
        }

        Ok(ResolvedEnqueue {
            argv,
            pools,
            executor: payload.executor.or_else(|| defaults.executor.clone()),
            priority: payload.priority.unwrap_or(defaults.priority),
            adapter,
            cwd: payload.cwd.or_else(|| defaults.cwd.clone()),
            workspace: payload.workspace.or_else(|| defaults.workspace.clone()),
            adapter_options: payload
                .adapter_options
                .unwrap_or_else(|| defaults.adapter_options.clone()),
            gate_manifest: payload.gate_manifest,
            brief_hash: None,
            resume_from: payload.resume_from,
            source,
            dedup_key: payload.dedup_key,
            orchestration: payload.orchestration,
            parent,
            evidence,
            drv,
            evidence_class: payload.evidence_class,
            manifest_hash: payload.manifest_hash,
            consumption_estimate: payload.consumption_estimate,
            runtime_max_sec: payload.runtime_max_sec,
            no_enqueue: payload.no_enqueue,
            credentials: payload.credentials,
            origin,
            gh_origin: payload.gh_origin,
            task_uuid: payload.task_uuid,
            related_trigger: payload.related_trigger,
            depth,
            wait: payload.wait,
        })
    }
}

fn validate_path(path: &Path, label: &str) -> Result<(), WireError> {
    if !path.is_absolute() {
        return Err(WireError::invalid(format!("{label} must be absolute")));
    }
    let path = path
        .to_str()
        .ok_or_else(|| WireError::invalid(format!("{label} must be valid UTF-8")))?;
    if path.contains('%') || path.contains('\0') || path.chars().any(char::is_control) {
        return Err(WireError::invalid(format!(
            "{label} must contain no control characters or systemd specifiers"
        )));
    }
    Ok(())
}

fn validate_credentials(credentials: &BTreeMap<String, PathBuf>) -> Result<(), WireError> {
    for (name, source) in credentials {
        let valid_name = !name.is_empty()
            && name.len() <= 255
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if !valid_name {
            return Err(WireError::invalid(format!(
                "invalid credential name {name:?}"
            )));
        }
        let Some(source) = source.to_str() else {
            return Err(WireError::invalid(format!(
                "credential {name:?} path must be valid UTF-8"
            )));
        };
        if !Path::new(source).is_absolute()
            || source.contains('%')
            || source.chars().any(char::is_control)
        {
            return Err(WireError::invalid(format!(
                "credential {name:?} path must be absolute and valid for systemd"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::time::Duration;

    use proptest::collection::vec;
    use proptest::prelude::*;
    use proptest::test_runner::{FileFailurePersistence, TestRunner};
    use tally_client::RpcClient;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;
    use tokio::sync::{mpsc, Semaphore};

    use super::*;
    use crate::pagination::PageCache;
    use crate::watch::{change_cursor, ChangeKind, ChangeStore};

    #[derive(Clone, Copy)]
    struct EchoHandler;

    impl RpcHandler for EchoHandler {
        fn handle<'a>(
            &'a self,
            request: RequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
            Box::pin(async move {
                if request.method == "missing" {
                    Err(WireError::not_found("missing object"))
                } else {
                    Ok(request.params.unwrap_or(Value::Null))
                }
            })
        }
    }

    #[test]
    fn arbitrary_byte_streams_never_panic_the_server() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        let mut runner = TestRunner::new(ProptestConfig {
            failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
                "crates/tally-core/proptest-regressions/wire.txt",
            ))),
            ..ProptestConfig::default()
        });
        runner
            .run(
                &(vec(any::<u8>(), 0..512), 1_u64..=128),
                |(input, max_frame_bytes)| {
                    runtime.block_on(local.run_until(async {
                        let (mut client, server) = UnixStream::pair().unwrap();
                        let task = tokio::task::spawn_local(async move {
                            serve_connection_with_max_frame_bytes(
                                server,
                                EchoHandler,
                                max_frame_bytes,
                            )
                            .await
                        });

                        let _ = client.write_all(&input).await;
                        let _ = client.shutdown().await;
                        let mut responses = Vec::new();
                        let (_, joined) = tokio::join!(client.read_to_end(&mut responses), task);
                        prop_assert!(joined.is_ok(), "server task panicked");
                        Ok(())
                    }))
                },
            )
            .unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rpc_is_one_correlated_ndjson_frame_per_line() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, EchoHandler).await.unwrap();
                });
                let client = RpcClient::connect(&socket).await.unwrap();
                let value = client
                    .call("echo", Some(serde_json::json!({"value": 42})))
                    .await
                    .unwrap();
                assert_eq!(value, serde_json::json!({"value": 42}));
                let error = client.call("missing", None).await.unwrap_err();
                assert!(matches!(
                    error,
                    WireIoError::Rpc(WireErrorCode::NotFound, _, _)
                ));
            })
            .await;
    }

    #[derive(Clone)]
    struct MultiplexHandler {
        started: mpsc::UnboundedSender<u64>,
        completed: mpsc::UnboundedSender<u64>,
    }

    impl RpcHandler for MultiplexHandler {
        fn handle<'a>(
            &'a self,
            request: RequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
            Box::pin(async move {
                if request.method.starts_with("query.") {
                    return Ok(serde_json::json!({
                        "interleaved": true,
                        "method": request.method,
                    }));
                }
                let index = request
                    .params
                    .as_ref()
                    .and_then(|params| params["index"].as_u64())
                    .ok_or_else(|| WireError::invalid("missing index"))?;
                self.started.send(index).unwrap();
                tokio::time::sleep(Duration::from_millis((7 - index) * 25)).await;
                self.completed.send(index).unwrap();
                Ok(serde_json::json!({"index": index}))
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_concurrent_serving_correlates_six_awaits_and_queries() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (completed_tx, mut completed_rx) = mpsc::unbounded_channel();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let server = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(
                        stream,
                        MultiplexHandler {
                            started: started_tx,
                            completed: completed_tx,
                        },
                    )
                    .await
                    .unwrap();
                });
                let client = RpcClient::connect(&socket).await.unwrap();
                let calls = (0..6_u64)
                    .map(|index| {
                        let client = client.clone();
                        tokio::task::spawn_local(async move {
                            let response = client
                                .call("queue.await_job", Some(serde_json::json!({"index": index})))
                                .await
                                .unwrap();
                            (index, response["index"].as_u64().unwrap())
                        })
                    })
                    .collect::<Vec<_>>();
                for _ in 0..6 {
                    started_rx.recv().await.unwrap();
                }
                let (status, pools) = tokio::join!(
                    tokio::time::timeout(
                        Duration::from_millis(40),
                        client.call("query.status", Some(serde_json::json!({}))),
                    ),
                    tokio::time::timeout(
                        Duration::from_millis(40),
                        client.call("query.pools", Some(serde_json::json!({}))),
                    ),
                );
                let status = status
                    .expect("a status query must not queue behind blocked awaits")
                    .unwrap();
                let pools = pools
                    .expect("a pools query must not queue behind blocked awaits")
                    .unwrap();
                assert_eq!(status["method"], "query.status");
                assert_eq!(pools["method"], "query.pools");

                let mut completion_order = Vec::new();
                for _ in 0..6 {
                    completion_order.push(completed_rx.recv().await.unwrap());
                }
                let mut completed_set = completion_order.clone();
                completed_set.sort_unstable();
                assert_eq!(completed_set, (0..6).collect::<Vec<_>>());
                assert_ne!(
                    completion_order,
                    (0..6).collect::<Vec<_>>(),
                    "the fixture must deliver out of request order so ID correlation is exercised"
                );
                for call in calls {
                    let (requested, received) = call.await.unwrap();
                    assert_eq!(received, requested);
                }
                drop(client);
                server.await.unwrap();
            })
            .await;
    }

    #[derive(Clone)]
    struct WindowHandler {
        started: mpsc::UnboundedSender<u64>,
        permits: Arc<Semaphore>,
    }

    impl RpcHandler for WindowHandler {
        fn handle<'a>(
            &'a self,
            request: RequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
            Box::pin(async move {
                let index = request
                    .params
                    .as_ref()
                    .and_then(|params| params["index"].as_u64())
                    .ok_or_else(|| WireError::invalid("missing index"))?;
                self.started.send(index).unwrap();
                self.permits.acquire().await.unwrap().forget();
                Ok(serde_json::json!({"index": index}))
            })
        }
    }

    #[derive(Clone)]
    struct NeverCompletesHandler {
        started: mpsc::UnboundedSender<String>,
    }

    impl RpcHandler for NeverCompletesHandler {
        fn handle<'a>(
            &'a self,
            request: RequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
            self.started.send(request.method).unwrap();
            Box::pin(std::future::pending())
        }
    }

    #[test]
    fn abandonable_wait_classifier_is_exact() {
        assert!(is_abandonable_wait("queue.await_job"));
        assert!(is_abandonable_wait("queue.await_barrier"));
        assert!(!is_abandonable_wait("queue.enqueue"));
        assert!(!is_abandonable_wait("query.watch"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn abandoned_never_completing_await_ends_after_the_peer_probe() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (mut client, server_stream) = UnixStream::pair().unwrap();
                let server = tokio::task::spawn_local(async move {
                    serve_connection(
                        server_stream,
                        NeverCompletesHandler {
                            started: started_tx,
                        },
                    )
                    .await
                });

                let request = RequestFrame {
                    id: RequestId::Number(1),
                    method: "queue.await_job".to_owned(),
                    params: None,
                };
                let mut encoded = serde_json::to_vec(&request).unwrap();
                encoded.push(b'\n');
                client.write_all(&encoded).await.unwrap();
                assert_eq!(started_rx.recv().await.as_deref(), Some("queue.await_job"));

                drop(client);
                tokio::time::timeout(PEER_HANGUP_PROBE_INTERVAL, server)
                    .await
                    .expect("an abandoned await must end within one peer-probe interval")
                    .unwrap()
                    .unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn peer_probe_never_aborts_a_non_wait_request() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (mut client, server_stream) = UnixStream::pair().unwrap();
                let mut server = tokio::task::spawn_local(async move {
                    serve_connection(
                        server_stream,
                        NeverCompletesHandler {
                            started: started_tx,
                        },
                    )
                    .await
                });

                let request = RequestFrame {
                    id: RequestId::Number(1),
                    method: "query.watch".to_owned(),
                    params: None,
                };
                let mut encoded = serde_json::to_vec(&request).unwrap();
                encoded.push(b'\n');
                client.write_all(&encoded).await.unwrap();
                assert_eq!(started_rx.recv().await.as_deref(), Some("query.watch"));

                drop(client);
                assert!(
                    tokio::time::timeout(PEER_HANGUP_PROBE_INTERVAL * 2, &mut server)
                        .await
                        .is_err(),
                    "a non-wait request must not be aborted after reader EOF"
                );
                server.abort();
                assert!(server.await.unwrap_err().is_cancelled());
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn half_closed_client_keeps_its_pending_await_response() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let permits = Arc::new(Semaphore::new(0));
        let server_permits = Arc::clone(&permits);
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (mut client, server_stream) = UnixStream::pair().unwrap();
                let server = tokio::task::spawn_local(async move {
                    serve_connection(
                        server_stream,
                        WindowHandler {
                            started: started_tx,
                            permits: server_permits,
                        },
                    )
                    .await
                });

                let request = RequestFrame {
                    id: RequestId::Number(1),
                    method: "queue.await_barrier".to_owned(),
                    params: Some(serde_json::json!({"index": 9})),
                };
                let mut encoded = serde_json::to_vec(&request).unwrap();
                encoded.push(b'\n');
                client.write_all(&encoded).await.unwrap();
                assert_eq!(started_rx.recv().await, Some(9));
                client.shutdown().await.unwrap();

                tokio::time::sleep(PEER_HANGUP_PROBE_INTERVAL * 2).await;
                assert!(
                    !server.is_finished(),
                    "a write-half shutdown must not look like a full peer hangup"
                );

                permits.add_permits(1);
                let mut reader = BufReader::new(client);
                let response = read_line_limited(&mut reader, DEFAULT_MAX_FRAME_BYTES)
                    .await
                    .unwrap()
                    .unwrap();
                let response: Value = serde_json::from_slice(&response).unwrap();
                assert_eq!(response["id"], 1);
                assert_eq!(response["result"]["index"], 9);
                server.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn idle_connection_closes_after_the_configured_timeout() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (mut client, server) = UnixStream::pair().unwrap();
                let server = tokio::task::spawn_local(async move {
                    serve_connection_with_limits(
                        server,
                        EchoHandler,
                        DEFAULT_MAX_FRAME_BYTES,
                        Some(Duration::from_millis(25)),
                    )
                    .await
                });

                let mut byte = [0_u8; 1];
                let read = tokio::time::timeout(Duration::from_secs(1), client.read(&mut byte))
                    .await
                    .expect("an idle connection must close")
                    .unwrap();
                assert_eq!(read, 0);
                server.await.unwrap().unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pending_await_survives_beyond_the_idle_timeout() {
        const IDLE_TIMEOUT: Duration = Duration::from_millis(25);

        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let permits = Arc::new(Semaphore::new(0));
        let server_permits = Arc::clone(&permits);
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (mut client, server_stream) = UnixStream::pair().unwrap();
                let server = tokio::task::spawn_local(async move {
                    serve_connection_with_limits(
                        server_stream,
                        WindowHandler {
                            started: started_tx,
                            permits: server_permits,
                        },
                        DEFAULT_MAX_FRAME_BYTES,
                        Some(IDLE_TIMEOUT),
                    )
                    .await
                });

                let request = RequestFrame {
                    id: RequestId::Number(1),
                    method: "queue.await_job".to_owned(),
                    params: Some(serde_json::json!({"index": 7})),
                };
                let mut encoded = serde_json::to_vec(&request).unwrap();
                encoded.push(b'\n');
                client.write_all(&encoded).await.unwrap();
                assert_eq!(started_rx.recv().await, Some(7));

                tokio::time::sleep(IDLE_TIMEOUT * 3).await;
                assert!(
                    !server.is_finished(),
                    "an in-flight await must suppress the idle timeout"
                );

                permits.add_permits(1);
                let mut reader = BufReader::new(client);
                let response = tokio::time::timeout(
                    Duration::from_secs(1),
                    read_line_limited(&mut reader, DEFAULT_MAX_FRAME_BYTES),
                )
                .await
                .expect("the pending await must complete")
                .unwrap()
                .unwrap();
                let response: Value = serde_json::from_slice(&response).unwrap();
                assert_eq!(response["id"], 1);
                assert_eq!(response["result"]["index"], 7);

                tokio::time::timeout(Duration::from_secs(1), server)
                    .await
                    .expect("the idle timeout must resume after the await")
                    .unwrap()
                    .unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_in_flight_window_is_64_with_fifo_overflow() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let permits = Arc::new(Semaphore::new(0));
        let server_permits = Arc::clone(&permits);
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let server = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(
                        stream,
                        WindowHandler {
                            started: started_tx,
                            permits: server_permits,
                        },
                    )
                    .await
                    .unwrap();
                });
                let stream = UnixStream::connect(&socket).await.unwrap();
                let (read_half, mut write_half) = stream.into_split();
                for index in 0..=MAX_IN_FLIGHT_REQUESTS {
                    let request = RequestFrame {
                        id: RequestId::Number(index as i64),
                        method: "queue.await_job".to_owned(),
                        params: Some(serde_json::json!({"index": index})),
                    };
                    let mut encoded = serde_json::to_vec(&request).unwrap();
                    encoded.push(b'\n');
                    write_half.write_all(&encoded).await.unwrap();
                }
                write_half.shutdown().await.unwrap();

                for expected in 0..MAX_IN_FLIGHT_REQUESTS as u64 {
                    assert_eq!(started_rx.recv().await, Some(expected));
                }
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), started_rx.recv())
                        .await
                        .is_err(),
                    "request 65 entered before an in-flight slot opened"
                );
                permits.add_permits(1);
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                        .await
                        .unwrap(),
                    Some(MAX_IN_FLIGHT_REQUESTS as u64)
                );
                permits.add_permits(MAX_IN_FLIGHT_REQUESTS);

                let mut reader = BufReader::new(read_half);
                let mut responses = 0;
                while read_line_limited(&mut reader, DEFAULT_MAX_FRAME_BYTES)
                    .await
                    .unwrap()
                    .is_some()
                {
                    responses += 1;
                }
                assert_eq!(responses, MAX_IN_FLIGHT_REQUESTS + 1);
                server.await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_default_frame_boundary_is_symmetric() {
        assert_eq!(DEFAULT_MAX_FRAME_BYTES, 16 * 1024 * 1024);
        let limit = DEFAULT_MAX_FRAME_BYTES as usize;

        let exact = Value::String("x".repeat(limit - 3));
        let mut exact_wire = Vec::new();
        write_frame(&mut exact_wire, &exact, DEFAULT_MAX_FRAME_BYTES)
            .await
            .unwrap();
        assert_eq!(exact_wire.len(), limit);
        let mut exact_reader = BufReader::new(exact_wire.as_slice());
        let exact_read = read_line_limited(&mut exact_reader, DEFAULT_MAX_FRAME_BYTES)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(exact_read.len(), limit - 1);
        assert_eq!(serde_json::from_slice::<Value>(&exact_read).unwrap(), exact);

        let oversized = Value::String("x".repeat(limit - 2));
        let mut sink = Vec::new();
        assert!(matches!(
            write_frame(&mut sink, &oversized, DEFAULT_MAX_FRAME_BYTES).await,
            Err(WireIoError::FrameTooLarge {
                limit: DEFAULT_MAX_FRAME_BYTES
            })
        ));
        assert!(sink.is_empty());

        let mut oversized_wire = vec![b'x'; limit];
        oversized_wire.push(b'\n');
        let mut oversized_reader = BufReader::new(oversized_wire.as_slice());
        assert!(matches!(
            read_line_limited(&mut oversized_reader, DEFAULT_MAX_FRAME_BYTES).await,
            Err(WireIoError::FrameTooLarge {
                limit: DEFAULT_MAX_FRAME_BYTES
            })
        ));
    }

    #[derive(Clone, Copy)]
    struct SizedFrameHandler;

    impl RpcHandler for SizedFrameHandler {
        fn handle<'a>(
            &'a self,
            request: RequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
            Box::pin(async move {
                match request.method.as_str() {
                    "ping" => Ok(serde_json::json!({"pong": true})),
                    "echo" => Ok(request.params.unwrap_or(Value::Null)),
                    "sized-response" => {
                        let target = request
                            .params
                            .as_ref()
                            .and_then(|params| params["wireBytes"].as_u64())
                            .ok_or_else(|| WireError::invalid("missing wireBytes"))?
                            as usize;
                        let empty = serde_json::to_vec(&ResponseOk {
                            id: &request.id,
                            result: Value::String(String::new()),
                        })
                        .unwrap()
                        .len()
                            + 1;
                        let result = Value::String("x".repeat(target.checked_sub(empty).unwrap()));
                        assert_eq!(
                            serde_json::to_vec(&ResponseOk {
                                id: &request.id,
                                result: result.clone(),
                            })
                            .unwrap()
                            .len()
                                + 1,
                            target
                        );
                        Ok(result)
                    }
                    _ => Err(WireError::not_found("missing frame fixture")),
                }
            })
        }
    }

    fn request_string_for_wire_size(id: &str, method: &str, target: usize) -> Value {
        let empty = RequestFrame {
            id: RequestId::String(id.to_owned()),
            method: method.to_owned(),
            params: Some(Value::String(String::new())),
        };
        let overhead = serde_json::to_vec(&empty).unwrap().len() + 1;
        let params = Value::String("x".repeat(target.checked_sub(overhead).unwrap()));
        let request = RequestFrame {
            id: RequestId::String(id.to_owned()),
            method: method.to_owned(),
            params: Some(params.clone()),
        };
        assert_eq!(serde_json::to_vec(&request).unwrap().len() + 1, target);
        params
    }

    async fn frame_connection(
        root: &Path,
        name: &str,
        server_limit: u64,
        client_limit: u64,
    ) -> (RpcClient, tokio::task::JoinHandle<Result<(), WireIoError>>) {
        let socket = root.join(format!("{name}.sock"));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::task::spawn_local(async move {
            let (stream, _) = listener.accept().await.unwrap();
            serve_connection_with_max_frame_bytes(stream, SizedFrameHandler, server_limit).await
        });
        let client = RpcClient::connect_with_max_frame_bytes(&socket, client_limit)
            .await
            .unwrap();
        (client, server)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_configured_frame_limits_are_symmetric_without_negotiation() {
        const LIMIT: u64 = 1_024;
        let temp = tempfile::tempdir().unwrap();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (client, server) =
                    frame_connection(temp.path(), "exact-request", LIMIT, LIMIT).await;
                let exact =
                    request_string_for_wire_size("cli-1", "echo", LIMIT.try_into().unwrap());
                assert_eq!(
                    client.call("echo", Some(exact.clone())).await.unwrap(),
                    exact
                );
                drop(client);
                server.await.unwrap().unwrap();

                let (client, server) =
                    frame_connection(temp.path(), "client-request-reject", LIMIT, LIMIT).await;
                let oversized = request_string_for_wire_size("cli-1", "echo", (LIMIT + 1) as usize);
                assert!(matches!(
                    client.call("echo", Some(oversized)).await,
                    Err(WireIoError::FrameTooLarge { limit: LIMIT })
                ));
                drop(client);
                server.await.unwrap().unwrap();

                let (client, server) =
                    frame_connection(temp.path(), "server-request-reject", LIMIT, LIMIT * 2).await;
                assert_eq!(client.call("ping", None).await.unwrap()["pong"], true);
                let oversized = request_string_for_wire_size("cli-2", "echo", (LIMIT + 1) as usize);
                assert!(matches!(
                    client.call("echo", Some(oversized)).await,
                    Err(WireIoError::Closed)
                ));
                drop(client);
                assert!(matches!(
                    server.await.unwrap(),
                    Err(WireIoError::FrameTooLarge { limit: LIMIT })
                ));

                let (client, server) =
                    frame_connection(temp.path(), "exact-response", LIMIT, LIMIT).await;
                let exact = client
                    .call(
                        "sized-response",
                        Some(serde_json::json!({"wireBytes": LIMIT})),
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    serde_json::to_vec(&ResponseOk {
                        id: &RequestId::String("cli-1".to_owned()),
                        result: exact,
                    })
                    .unwrap()
                    .len()
                        + 1,
                    LIMIT as usize
                );
                drop(client);
                server.await.unwrap().unwrap();

                let (client, server) =
                    frame_connection(temp.path(), "client-response-reject", LIMIT * 2, LIMIT).await;
                assert_eq!(client.call("ping", None).await.unwrap()["pong"], true);
                assert!(matches!(
                    client
                        .call(
                            "sized-response",
                            Some(serde_json::json!({"wireBytes": LIMIT + 1})),
                        )
                        .await,
                    Err(WireIoError::FrameTooLarge { limit: LIMIT })
                ));
                drop(client);
                server.await.unwrap().unwrap();

                let (client, server) =
                    frame_connection(temp.path(), "server-response-reject", LIMIT, LIMIT * 2).await;
                assert!(matches!(
                    client
                        .call(
                            "sized-response",
                            Some(serde_json::json!({"wireBytes": LIMIT + 1})),
                        )
                        .await,
                    Err(WireIoError::Closed)
                ));
                drop(client);
                assert!(matches!(
                    server.await.unwrap(),
                    Err(WireIoError::FrameTooLarge { limit: LIMIT })
                ));
            })
            .await;
    }

    #[derive(Clone)]
    struct ConcurrentObservationHandler {
        started: mpsc::UnboundedSender<u64>,
        permits: Arc<Semaphore>,
        page_cache: Rc<RefCell<PageCache>>,
        page_items: Rc<Vec<Value>>,
        changes: Rc<ChangeStore>,
    }

    impl RpcHandler for ConcurrentObservationHandler {
        fn handle<'a>(
            &'a self,
            request: RequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
            Box::pin(async move {
                match request.method.as_str() {
                    "queue.await_job" => {
                        let index = request
                            .params
                            .as_ref()
                            .and_then(|params| params["index"].as_u64())
                            .ok_or_else(|| WireError::invalid("missing index"))?;
                        self.started.send(index).unwrap();
                        self.permits.acquire().await.unwrap().forget();
                        Ok(serde_json::json!({"index": index}))
                    }
                    "query.watch" => {
                        let params = request.params.as_ref().unwrap();
                        let after = params["after"].as_str();
                        let limit = params["limit"].as_u64().map(|value| value as usize);
                        serde_json::to_value(self.changes.watch(after, limit).map_err(|error| {
                            WireError::new(WireErrorCode::Internal, error.to_string())
                        })?)
                        .map_err(|error| WireError::new(WireErrorCode::Internal, error.to_string()))
                    }
                    "query.jobs" => {
                        let params = request.params.as_ref().unwrap();
                        let cursor = params["cursor"].as_str();
                        let envelope = cursor.is_none().then(|| {
                            serde_json::json!({
                                "schemaVersion": 1,
                                "protocolVersion": 5,
                                "items": self.page_items.as_ref().clone(),
                                "nextCursor": null,
                            })
                        });
                        self.page_cache
                            .borrow_mut()
                            .page(
                                "query.jobs",
                                "fleet-conformance",
                                Some(1_000),
                                cursor,
                                envelope,
                            )
                            .map_err(|error| {
                                WireError::new(WireErrorCode::Internal, error.to_string())
                            })
                    }
                    "query.status" => Ok(serde_json::json!({"interleaved": true})),
                    _ => Err(WireError::not_found("missing observation fixture")),
                }
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn fleet_conformance_watch_and_pagination_are_exact_under_concurrency() {
        const RECORDS: u64 = 1_200;
        const PAGE_CAP: usize = 48 * 1024;

        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut changes =
            ChangeStore::open_with_capacity(&temp.path().join("changes"), 2_000).unwrap();
        for index in 0..RECORDS {
            changes
                .append_now(
                    ChangeKind::Lifecycle,
                    serde_json::json!({"index": index, "raw": "x".repeat(96)}),
                )
                .unwrap();
        }
        let page_items = Rc::new(
            (0..RECORDS)
                .map(|index| serde_json::json!({"index": index, "raw": "x".repeat(96)}))
                .collect::<Vec<_>>(),
        );
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let permits = Arc::new(Semaphore::new(0));
        let handler = ConcurrentObservationHandler {
            started: started_tx,
            permits: Arc::clone(&permits),
            page_cache: Rc::new(RefCell::new(PageCache::default())),
            page_items,
            changes: Rc::new(changes),
        };
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async move {
                let server = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(stream, handler).await.unwrap();
                });
                let client = RpcClient::connect(&socket).await.unwrap();
                let awaits = (0..6_u64)
                    .map(|index| {
                        let client = client.clone();
                        tokio::task::spawn_local(async move {
                            client
                                .call("queue.await_job", Some(serde_json::json!({"index": index})))
                                .await
                                .unwrap()
                        })
                    })
                    .collect::<Vec<_>>();
                for _ in 0..6 {
                    started_rx.recv().await.unwrap();
                }
                let status = tokio::time::timeout(
                    Duration::from_secs(1),
                    client.call("query.status", Some(serde_json::json!({}))),
                )
                .await
                .expect("status must complete while six awaits are blocked")
                .unwrap();
                assert_eq!(status["interleaved"], true);

                let mut watch_cursor = change_cursor(0);
                let mut watched = Vec::new();
                while watched.len() < RECORDS as usize {
                    let page = tokio::time::timeout(
                        Duration::from_secs(1),
                        client.call(
                            "query.watch",
                            Some(serde_json::json!({
                                "after": watch_cursor,
                                "limit": 1_000,
                            })),
                        ),
                    )
                    .await
                    .expect("watch must complete while awaits are blocked")
                    .unwrap();
                    assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_CAP);
                    watched.extend(
                        page["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|item| item["payload"]["index"].as_u64().unwrap()),
                    );
                    watch_cursor = page["nextCursor"].as_str().unwrap().to_owned();
                }
                assert_eq!(watched, (0..RECORDS).collect::<Vec<_>>());

                let mut page_cursor = None;
                let mut paged = Vec::new();
                loop {
                    let page = tokio::time::timeout(
                        Duration::from_secs(1),
                        client.call(
                            "query.jobs",
                            Some(serde_json::json!({"cursor": page_cursor})),
                        ),
                    )
                    .await
                    .expect("pagination must complete while awaits are blocked")
                    .unwrap();
                    assert!(serde_json::to_vec(&page).unwrap().len() <= PAGE_CAP);
                    paged.extend(
                        page["items"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|item| item["index"].as_u64().unwrap()),
                    );
                    page_cursor = page["nextCursor"].as_str().map(ToOwned::to_owned);
                    if page_cursor.is_none() {
                        break;
                    }
                }
                assert_eq!(paged, (0..RECORDS).collect::<Vec<_>>());

                permits.add_permits(6);
                for (index, await_call) in awaits.into_iter().enumerate() {
                    assert_eq!(
                        await_call.await.unwrap()["index"],
                        serde_json::json!(index as u64)
                    );
                }
                drop(client);
                server.await.unwrap();
            })
            .await;
    }

    #[test]
    fn request_serialization_is_byte_stable() {
        let request = RequestFrame {
            id: RequestId::String("cli-1".to_owned()),
            method: "query.status".to_owned(),
            params: Some(serde_json::json!({"pool": "gpu"})),
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"id":"cli-1","method":"query.status","params":{"pool":"gpu"}}"#
        );
    }

    #[test]
    fn enqueue_pool_wire_compatibility_is_scalar_for_singletons_and_array_for_multi() {
        let mut legacy = child_payload();
        legacy.caller_job_id = None;
        legacy.pools = Some(vec!["slot".to_owned()]);
        let encoded = serde_json::to_value(&legacy).unwrap();
        assert_eq!(encoded["pool"], "slot");
        let decoded: EnqueuePayload = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.pools, Some(vec!["slot".to_owned()]));

        legacy.pools = Some(vec!["slot".to_owned(), "zeta".to_owned()]);
        let encoded = serde_json::to_value(&legacy).unwrap();
        assert_eq!(encoded["pool"], serde_json::json!(["slot", "zeta"]));
        let decoded: EnqueuePayload = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.pools, legacy.pools);
    }

    #[test]
    fn legacy_github_actor_wire_name_maps_to_trigger_actor() {
        let mut encoded = serde_json::to_value(child_payload()).unwrap();
        encoded.as_object_mut().unwrap().remove("ghTriggerActor");
        encoded["ghActor"] = Value::String("legacy-trigger".to_owned());
        let decoded: EnqueuePayload = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.gh_trigger_actor.as_deref(), Some("legacy-trigger"));
    }

    #[test]
    fn enqueue_pool_set_rejections_are_actionable_and_canonicalization_is_stable() {
        let mut state = GuardrailState::new(GuardrailConfig::default()).unwrap();
        let mut payload = child_payload();
        payload.caller_job_id = None;
        payload.pools = Some(Vec::new());
        assert!(state
            .validate_enqueue(payload.clone(), &defaults())
            .unwrap_err()
            .message
            .contains("at least one"));

        payload.pools = Some(vec!["slot".to_owned(), "slot".to_owned()]);
        assert!(state
            .validate_enqueue(payload.clone(), &defaults())
            .unwrap_err()
            .message
            .contains("duplicate"));

        payload.pools = Some(vec!["zeta".to_owned(), "alpha".to_owned()]);
        assert_eq!(
            state.validate_enqueue(payload, &defaults()).unwrap().pools,
            ["alpha", "zeta"]
        );
    }

    #[test]
    fn quote_aware_split_is_direct_exec_only() {
        assert_eq!(
            split_invocation(r#"cmd "two words" 'three four' escaped\ space """#).unwrap(),
            ["cmd", "two words", "three four", "escaped space", ""]
        );
        assert!(split_invocation("cmd 'unterminated").is_err());
        assert_eq!(
            split_invocation("cmd > literal").unwrap(),
            ["cmd", ">", "literal"]
        );
    }

    #[test]
    fn explicit_argv_preserves_empty_arguments() {
        let mut state = GuardrailState::new(GuardrailConfig::default()).unwrap();
        let mut payload = child_payload();
        payload.argv = Some(vec![
            "work".to_owned(),
            String::new(),
            "--literal".to_owned(),
        ]);
        payload.invocation = None;
        payload.caller_job_id = None;
        let resolved = state.validate_enqueue(payload, &defaults()).unwrap();
        assert_eq!(resolved.argv, ["work", "", "--literal"]);
    }

    fn defaults() -> ProducerDefaults {
        ProducerDefaults {
            pools: vec!["default-pool".to_owned()],
            executor: None,
            priority: Priority::Low,
            adapter: "shell".to_owned(),
            source: EnqueueSource::Calendar,
            cwd: None,
            workspace: None,
            adapter_options: AdapterJobOptions::default(),
        }
    }

    fn child_payload() -> EnqueuePayload {
        EnqueuePayload {
            invocation: None,
            argv: Some(vec!["child".to_owned()]),
            pools: None,
            executor: None,
            priority: None,
            adapter: None,
            cwd: None,
            workspace: None,
            adapter_options: None,
            gate_manifest: None,
            brief: None,
            brief_path: None,
            resume_from: None,
            source: None,
            dedup_key: Some("child-1".to_owned()),
            submission: None,
            orchestration: None,
            parent: None,
            evidence: Vec::new(),
            drv: None,
            evidence_class: None,
            manifest_hash: None,
            consumption_estimate: None,
            runtime_max_sec: None,
            no_enqueue: false,
            credentials: BTreeMap::new(),
            origin: None,
            caller_job_id: Some("job-parent".to_owned()),
            caller_job_token: None,
            gh_trigger_actor: None,
            gh_self_actor: None,
            gh_origin: None,
            task_uuid: None,
            related_trigger: None,
            wait: false,
        }
    }

    #[test]
    fn caller_job_token_is_additive_and_uses_the_camel_case_wire_key() {
        let mut payload = child_payload();
        let legacy = serde_json::to_value(&payload).unwrap();
        assert!(legacy.get("callerJobToken").is_none());

        payload.caller_job_token = Some("ab".repeat(32));
        let encoded = serde_json::to_value(&payload).unwrap();
        assert_eq!(encoded["callerJobToken"], "ab".repeat(32));
        assert_eq!(
            serde_json::from_value::<EnqueuePayload>(encoded)
                .unwrap()
                .caller_job_token,
            payload.caller_job_token
        );
    }

    #[test]
    fn full_mode_flow_rejects_kernel_only_hashed_fields() {
        let mut payload = child_payload();
        payload.caller_job_id = None;
        payload.submission = Some(SubmissionOptions {
            mode: SubmissionMode::Full,
        });
        payload.orchestration = Some(
            serde_json::from_value(serde_json::json!({
                "flowRunId": "00000000-0000-4000-8000-000000000145",
                "maxNodes": 1
            }))
            .unwrap(),
        );

        let mut with_cwd = payload.clone();
        with_cwd.cwd = Some(PathBuf::from("/work/flow"));
        assert_eq!(
            GuardrailState::new(GuardrailConfig::default())
                .unwrap()
                .validate_enqueue(with_cwd, &defaults())
                .unwrap_err()
                .message,
            "full-mode flow submissions require cwd to be absent until NodeSpec exposes it"
        );

        payload.gate_manifest = Some(
            serde_json::from_value(serde_json::json!({
                "path": "/work/flow/gates.json",
                "requiredGateIds": ["tests"],
                "acceptancePolicy": "manual"
            }))
            .unwrap(),
        );
        assert_eq!(
            GuardrailState::new(GuardrailConfig::default())
                .unwrap()
                .validate_enqueue(payload, &defaults())
                .unwrap_err()
                .message,
            "full-mode flow submissions require gateManifest to be absent until NodeSpec exposes it"
        );
    }

    #[test]
    fn child_is_auto_parented_and_payload_overrides_defaults() {
        let mut state = GuardrailState::new(GuardrailConfig {
            depth_cap: 3,
            fanout_cap: 2,
            require_dedup_key: true,
        })
        .unwrap();
        state.register_parent(
            "job-parent",
            ParentInfo {
                parent_uuid: "task-parent".to_owned(),
                depth: 1,
                outstanding: 0,
                no_enqueue: false,
                terminal: false,
            },
        );
        let mut payload = child_payload();
        payload.pools = Some(vec!["payload-pool".to_owned()]);
        payload.priority = Some(Priority::Interrupt);
        payload.adapter = Some("codex".to_owned());
        let resolved = state.validate_enqueue(payload, &defaults()).unwrap();
        assert_eq!(resolved.parent.as_deref(), Some("task-parent"));
        assert_eq!(resolved.depth, 2);
        assert_eq!(resolved.pools, ["payload-pool"]);
        assert_eq!(resolved.priority, Priority::Interrupt);
        assert_eq!(resolved.adapter, "codex");
    }

    #[test]
    fn opaque_admission_metadata_is_passed_through_verbatim() {
        let mut state = GuardrailState::new(GuardrailConfig::default()).unwrap();
        let mut payload = child_payload();
        payload.caller_job_id = None;
        let evidence_class = serde_json::json!({
            "arbitrary": [true, 7, {"nested": null}],
            "label": "opaque"
        });
        let manifest_hash = "deliberately-not-validated://manifest value".to_owned();
        payload.evidence_class = Some(evidence_class.clone());
        payload.manifest_hash = Some(manifest_hash.clone());

        let resolved = state.validate_enqueue(payload, &defaults()).unwrap();
        assert_eq!(resolved.evidence_class, Some(evidence_class));
        assert_eq!(resolved.manifest_hash, Some(manifest_hash));
    }

    #[test]
    fn fallback_enqueue_preserves_related_trigger_without_falsifying_source() {
        let mut state = GuardrailState::new(GuardrailConfig::default()).unwrap();
        let related = RelatedTrigger {
            producer: "github".to_owned(),
            event_id: "comment-5068723021".to_owned(),
            outcome: crate::taskdb::RelatedTriggerOutcome::Filtered,
            receipt_id: Some("receipt-84".to_owned()),
        };
        let mut payload = child_payload();
        payload.caller_job_id = None;
        payload.source = Some(EnqueueSource::Orchestrator);
        payload.related_trigger = Some(related.clone());
        let resolved = state.validate_enqueue(payload, &defaults()).unwrap();
        assert_eq!(resolved.source, EnqueueSource::Orchestrator);
        assert_eq!(resolved.related_trigger, Some(related.clone()));
        assert!(resolved.gh_origin.is_none());

        let mut dishonest = child_payload();
        dishonest.caller_job_id = None;
        dishonest.source = Some(EnqueueSource::Gh);
        dishonest.related_trigger = Some(related);
        assert!(state
            .validate_enqueue(dishonest, &defaults())
            .unwrap_err()
            .message
            .contains("fallback provenance"));
    }

    #[test]
    fn no_enqueue_depth_fanout_and_dedup_are_enforced() {
        let state = Rc::new(RefCell::new(
            GuardrailState::new(GuardrailConfig {
                depth_cap: 2,
                fanout_cap: 1,
                require_dedup_key: true,
            })
            .unwrap(),
        ));
        state.borrow_mut().register_parent(
            "job-parent",
            ParentInfo {
                parent_uuid: "task-parent".to_owned(),
                depth: 1,
                outstanding: 0,
                no_enqueue: false,
                terminal: false,
            },
        );
        let mut missing_dedup = child_payload();
        missing_dedup.dedup_key = None;
        assert!(state
            .borrow_mut()
            .validate_enqueue(missing_dedup, &defaults())
            .is_err());
        state
            .borrow_mut()
            .validate_enqueue(child_payload(), &defaults())
            .unwrap();
        assert!(state
            .borrow_mut()
            .validate_enqueue(child_payload(), &defaults())
            .unwrap_err()
            .message
            .contains("fanoutCap"));

        state.borrow_mut().register_parent(
            "job-deep",
            ParentInfo {
                parent_uuid: "task-deep".to_owned(),
                depth: 2,
                outstanding: 0,
                no_enqueue: false,
                terminal: false,
            },
        );
        let mut deep = child_payload();
        deep.caller_job_id = Some("job-deep".to_owned());
        assert!(state
            .borrow_mut()
            .validate_enqueue(deep, &defaults())
            .unwrap_err()
            .message
            .contains("depthCap"));

        state.borrow_mut().register_parent(
            "job-advisory",
            ParentInfo {
                parent_uuid: "task-advisory".to_owned(),
                depth: 0,
                outstanding: 0,
                no_enqueue: true,
                terminal: false,
            },
        );
        let mut advisory = child_payload();
        advisory.caller_job_id = Some("job-advisory".to_owned());
        assert!(state
            .borrow_mut()
            .validate_enqueue(advisory, &defaults())
            .unwrap_err()
            .message
            .contains("noEnqueue"));
    }

    #[test]
    fn credential_sources_are_rejected_before_enqueue_when_not_absolute() {
        let mut state = GuardrailState::new(GuardrailConfig::default()).unwrap();
        let mut payload = child_payload();
        payload
            .credentials
            .insert("token".to_owned(), PathBuf::from("relative/token"));
        assert!(state
            .validate_enqueue(payload, &defaults())
            .unwrap_err()
            .message
            .contains("must be absolute"));
    }

    #[test]
    fn github_self_actor_is_excluded() {
        let mut state = GuardrailState::new(GuardrailConfig::default()).unwrap();
        let mut payload = child_payload();
        payload.caller_job_id = None;
        payload.source = Some(EnqueueSource::Gh);
        payload.gh_trigger_actor = Some("bot".to_owned());
        payload.gh_self_actor = Some("bot".to_owned());
        assert!(state.validate_enqueue(payload, &defaults()).is_err());
    }

    #[test]
    fn enqueue_validates_and_canonicalizes_evidence_before_charging_fanout() {
        let mut state = GuardrailState::new(GuardrailConfig {
            depth_cap: 3,
            fanout_cap: 1,
            require_dedup_key: true,
        })
        .unwrap();
        state.register_parent(
            "job-parent",
            ParentInfo {
                parent_uuid: "task-parent".to_owned(),
                depth: 0,
                outstanding: 0,
                no_enqueue: false,
                terminal: false,
            },
        );

        let mut malformed = child_payload();
        malformed.evidence = vec!["hash:sha256:short".to_owned()];
        assert!(state.validate_enqueue(malformed, &defaults()).is_err());
        assert_eq!(state.parent("job-parent").unwrap().outstanding, 0);

        let mut valid = child_payload();
        valid.evidence = vec![
            "artifact:/tmp/output".to_owned(),
            format!("hash:sha256:{}", "A".repeat(64)),
            "exit:0".to_owned(),
        ];
        let resolved = state.validate_enqueue(valid, &defaults()).unwrap();
        assert_eq!(
            resolved.evidence,
            [
                "artifact:/tmp/output",
                &format!("hash:sha256:{}", "a".repeat(64)),
                "exit:0",
            ]
        );
        assert_eq!(state.parent("job-parent").unwrap().outstanding, 1);
        state.rollback_child_charge("job-parent").unwrap();
        assert_eq!(state.parent("job-parent").unwrap().outstanding, 0);
        state
            .validate_enqueue(child_payload(), &defaults())
            .expect("a failed post-validation admission can return its fanout charge");
    }

    #[test]
    fn consumption_estimate_cannot_be_negative() {
        let payload = serde_json::json!({
            "argv": ["true"],
            "pool": "api",
            "consumptionEstimate": -1
        });
        assert!(serde_json::from_value::<EnqueuePayload>(payload).is_err());
    }

    #[test]
    fn canonical_payload_is_exact_ordered_and_excludes_admission_metadata() {
        let mut payload = child_payload();
        payload.caller_job_id = None;
        payload.argv = Some(vec!["tool".to_owned(), "--flag".to_owned()]);
        payload.pools = Some(vec!["zeta".to_owned(), "alpha".to_owned()]);
        payload.executor = Some("worker".to_owned());
        payload.adapter = Some("codex".to_owned());
        payload.cwd = Some(PathBuf::from("/work/tree"));
        payload.workspace = Some(WorkspaceMetadata {
            repo: "acme/widgets".to_owned(),
            base_rev: "origin/main".to_owned(),
            branch: "fs-1".to_owned(),
            worktree_path: PathBuf::from("/work/tree"),
        });
        payload.adapter_options = Some(AdapterJobOptions {
            pre_prompt_argv: vec!["--json".to_owned()],
            environment: BTreeMap::from([("NO_COLOR".to_owned(), "1".to_owned())]),
            approval_policy: Some("never".to_owned()),
            sandbox_policy: None,
            model: Some("gpt-5".to_owned()),
            effort: None,
        });
        payload.gate_manifest = Some(GateManifestSpec {
            path: PathBuf::from("/work/tree/gates.json"),
            required_gate_ids: vec!["tests".to_owned()],
            acceptance_policy: Default::default(),
        });
        payload.evidence = vec!["exit:0".to_owned(), "artifact:/work/tree/out".to_owned()];
        payload.evidence_class = Some(serde_json::json!({"kind": "build", "level": 2}));
        payload.manifest_hash = Some("opaque-manifest".to_owned());
        payload.runtime_max_sec = Some(300);
        payload.no_enqueue = true;
        payload
            .credentials
            .insert("token".to_owned(), PathBuf::from("/run/credentials/token"));
        payload.priority = Some(Priority::Interrupt);
        payload.dedup_key = Some("excluded-key".to_owned());
        payload.consumption_estimate = Some(99);
        payload.wait = true;

        let mut state = GuardrailState::new(GuardrailConfig::default()).unwrap();
        let resolved = state.validate_enqueue(payload, &defaults()).unwrap();
        let canonical = String::from_utf8(canonical_payload(&resolved).unwrap()).unwrap();
        assert_eq!(
            canonical,
            concat!(
                r#"{"argv":["tool","--flag"],"pool":["alpha","zeta"],"executor":"worker","#,
                r#""adapter":"codex","cwd":"/work/tree","workspace":{"repo":"acme/widgets","#,
                r#""baseRev":"origin/main","branch":"fs-1","worktreePath":"/work/tree"},"#,
                r#""adapterOptions":{"prePromptArgv":["--json"],"environment":{"NO_COLOR":"1"},"#,
                r#""approvalPolicy":"never","model":"gpt-5"},"gateManifest":{"path":"#,
                r#""/work/tree/gates.json","requiredGateIds":["tests"],"acceptancePolicy":"manual"},"#,
                r#""evidence":["exit:0","artifact:/work/tree/out"],"evidenceClass":{"kind":"#,
                r#""build","level":2},"manifestHash":"opaque-manifest","runtimeMaxSec":300,"#,
                r#""noEnqueue":true,"credentials":{"token":"/run/credentials/token"}}"#
            )
        );
        assert_eq!(
            canonical_payload_hash(&resolved).unwrap(),
            "sha256:3c5f2f51481120aa7cf11dd05410a4b521e0cc25e793b8cd73a1a9d0fdd02fc4"
        );

        let mut metadata_only = resolved.clone();
        metadata_only.priority = Priority::Low;
        metadata_only.source = EnqueueSource::Manual;
        metadata_only.dedup_key = Some("another-key".to_owned());
        metadata_only.parent = Some("00000000-0000-4000-8000-000000000001".to_owned());
        metadata_only.consumption_estimate = Some(1);
        metadata_only.resume_from = Some("00000000-0000-4000-8000-000000000002".to_owned());
        metadata_only.task_uuid = Some("00000000-0000-4000-8000-000000000003".to_owned());
        metadata_only.orchestration = Some(
            serde_json::from_value(serde_json::json!({
                "flowRunId": "00000000-0000-4000-8000-000000000004",
                "maxNodes": 9,
                "opaque": {"member": "worker-a"}
            }))
            .unwrap(),
        );
        metadata_only.depth = 3;
        metadata_only.wait = false;
        assert_eq!(
            canonical_payload_hash(&metadata_only).unwrap(),
            canonical_payload_hash(&resolved).unwrap()
        );

        let mut brief_work = resolved.clone();
        brief_work.brief_hash = Some(format!("sha256:{}", "a".repeat(64)));
        let brief_canonical = String::from_utf8(canonical_payload(&brief_work).unwrap()).unwrap();
        assert!(
            brief_canonical.ends_with(&format!(r#","briefHash":"sha256:{}"}}"#, "a".repeat(64)))
        );
        assert_ne!(
            canonical_payload_hash(&brief_work).unwrap(),
            canonical_payload_hash(&resolved).unwrap()
        );

        metadata_only.argv.push("different-work".to_owned());
        assert_ne!(
            canonical_payload_hash(&metadata_only).unwrap(),
            canonical_payload_hash(&resolved).unwrap()
        );
    }

    #[test]
    fn full_submission_defers_fanout_charge_until_created_is_known() {
        let mut state = GuardrailState::new(GuardrailConfig {
            depth_cap: 3,
            fanout_cap: 1,
            require_dedup_key: true,
        })
        .unwrap();
        state.register_parent(
            "job-parent",
            ParentInfo {
                parent_uuid: "task-parent".to_owned(),
                depth: 0,
                outstanding: 1,
                no_enqueue: false,
                terminal: false,
            },
        );
        let mut payload = child_payload();
        payload.submission = Some(SubmissionOptions {
            mode: SubmissionMode::Full,
        });
        let resolved = state.validate_enqueue(payload, &defaults()).unwrap();
        assert_eq!(resolved.parent.as_deref(), Some("task-parent"));
        assert_eq!(state.parent("job-parent").unwrap().outstanding, 1);
        assert!(state.charge_child("job-parent").is_err());
    }
}
