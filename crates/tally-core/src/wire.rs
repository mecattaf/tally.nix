use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinSet;

use crate::adapters::AdapterJobOptions;
use crate::completion::GateManifestSpec;
use crate::config::{Priority, DEFAULT_MAX_FRAME_BYTES};
use crate::evidence::parse_evidence_specs;
use crate::provenance::Orchestration;
use crate::taskdb::{
    gh_trigger_task_uuid, AdmissionOrigin, EnqueueSource, GhOrigin, RelatedTrigger,
    WorkspaceMetadata,
};

pub const FRAME_CAP_BYTES: usize = DEFAULT_MAX_FRAME_BYTES as usize;
pub const MAX_IN_FLIGHT_REQUESTS: usize = 64;

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
    "lease.acquire",
    "lease.release",
    "lease.status",
    "query.jobs",
    "query.job",
    "query.status",
    "query.log",
    "query.proof",
    "query.trace",
    "query.producers",
    "query.watch",
    "query.render",
    "query.standup",
    "query.pools",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Number(i64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestFrame {
    pub id: RequestId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireErrorCode {
    UnsupportedProtocol,
    InvalidParams,
    InvalidFrame,
    FrameTooLarge,
    UnknownMethod,
    NotFound,
    Unsupported,
    Internal,
    Timeout,
    EpochChanged,
    #[serde(rename = "dedup-key-conflict")]
    DedupKeyConflict,
    #[serde(rename = "flow-node-cap")]
    FlowNodeCap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireError {
    pub code: WireErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl WireError {
    pub fn new(code: WireErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::InvalidParams, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(WireErrorCode::NotFound, message)
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

#[derive(Debug, Error)]
pub enum WireIoError {
    #[error("daemon socket {path} is unreachable: {source}")]
    Unreachable { path: PathBuf, source: io::Error },
    #[error("wire I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("wire JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("wire frame exceeds {limit} bytes")]
    FrameTooLarge { limit: u64 },
    #[error("wire frame limit must be positive")]
    InvalidFrameLimit,
    #[error("daemon closed the socket before replying")]
    Closed,
    #[error("invalid response frame: {0}")]
    InvalidResponse(String),
    #[error("RPC request task failed: {0}")]
    RequestTask(String),
    #[error("RPC error {0:?}: {1}")]
    Rpc(WireErrorCode, String, Option<Value>),
}

pub trait RpcHandler {
    fn handle<'a>(
        &'a self,
        request: RequestFrame,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>>;
}

async fn read_line_limited<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max_frame_bytes: u64,
) -> Result<Option<Vec<u8>>, WireIoError> {
    validate_frame_limit(max_frame_bytes)?;
    let mut line = Vec::new();
    loop {
        let (take, complete, eof) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                (0, false, true)
            } else if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                (newline + 1, true, false)
            } else {
                (available.len(), false, false)
            }
        };
        if eof {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let next_len =
            (line.len() as u64)
                .checked_add(take as u64)
                .ok_or(WireIoError::FrameTooLarge {
                    limit: max_frame_bytes,
                })?;
        if next_len > max_frame_bytes {
            return Err(WireIoError::FrameTooLarge {
                limit: max_frame_bytes,
            });
        }
        let available = reader.fill_buf().await?;
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if complete {
            break;
        }
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

pub async fn serve_connection<H>(stream: UnixStream, handler: H) -> Result<(), WireIoError>
where
    H: RpcHandler + Clone + 'static,
{
    serve_connection_with_max_frame_bytes(stream, handler, DEFAULT_MAX_FRAME_BYTES).await
}

pub async fn serve_connection_with_max_frame_bytes<H>(
    stream: UnixStream,
    handler: H,
    max_frame_bytes: u64,
) -> Result<(), WireIoError>
where
    H: RpcHandler + Clone + 'static,
{
    validate_frame_limit(max_frame_bytes)?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut reader_open = true;
    let mut requests = JoinSet::new();
    while reader_open || !requests.is_empty() {
        if !reader_open || requests.len() == MAX_IN_FLIGHT_REQUESTS {
            let completed = requests
                .join_next()
                .await
                .expect("an in-flight request must be present");
            write_completed_request(&mut writer, completed, max_frame_bytes).await?;
            continue;
        }

        tokio::select! {
            line = read_line_limited(&mut reader, max_frame_bytes) => {
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
                requests.spawn_local(async move {
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
                });
            }
            completed = requests.join_next(), if !requests.is_empty() => {
                let completed = completed.expect("an in-flight request must be present");
                write_completed_request(&mut writer, completed, max_frame_bytes).await?;
            }
        }
    }
    Ok(())
}

async fn write_completed_request(
    writer: &mut OwnedWriteHalf,
    completed: Result<Result<Value, WireIoError>, tokio::task::JoinError>,
    max_frame_bytes: u64,
) -> Result<(), WireIoError> {
    let response = completed.map_err(|error| WireIoError::RequestTask(error.to_string()))??;
    write_frame(writer, &response, max_frame_bytes).await
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

fn validate_frame_limit(max_frame_bytes: u64) -> Result<(), WireIoError> {
    if max_frame_bytes == 0 {
        Err(WireIoError::InvalidFrameLimit)
    } else {
        Ok(())
    }
}

fn ensure_frame_size(encoded_bytes: usize, max_frame_bytes: u64) -> Result<(), WireIoError> {
    let frame_bytes = (encoded_bytes as u64)
        .checked_add(1)
        .ok_or(WireIoError::FrameTooLarge {
            limit: max_frame_bytes,
        })?;
    if frame_bytes > max_frame_bytes {
        return Err(WireIoError::FrameTooLarge {
            limit: max_frame_bytes,
        });
    }
    Ok(())
}

type PendingResponse = oneshot::Sender<Result<Value, ClientReadFailure>>;

#[derive(Debug, Clone)]
enum ClientReadFailure {
    FrameTooLarge { limit: u64 },
    Closed,
    Io(String),
    Json(String),
    InvalidResponse(String),
}

impl ClientReadFailure {
    fn into_wire_error(self) -> WireIoError {
        match self {
            Self::FrameTooLarge { limit } => WireIoError::FrameTooLarge { limit },
            Self::Closed => WireIoError::Closed,
            Self::Io(message) => WireIoError::Io(io::Error::other(message)),
            Self::Json(message) => {
                WireIoError::InvalidResponse(format!("response is not valid JSON: {message}"))
            }
            Self::InvalidResponse(message) => WireIoError::InvalidResponse(message),
        }
    }
}

#[derive(Default)]
struct ClientState {
    pending: HashMap<RequestId, PendingResponse>,
    failure: Option<ClientReadFailure>,
}

#[derive(Clone)]
pub struct RpcClient {
    writer: Arc<Mutex<OwnedWriteHalf>>,
    state: Arc<StdMutex<ClientState>>,
    next_id: Arc<AtomicU64>,
    max_frame_bytes: u64,
}

impl RpcClient {
    pub async fn connect(path: &Path) -> Result<Self, WireIoError> {
        Self::connect_with_max_frame_bytes(path, DEFAULT_MAX_FRAME_BYTES).await
    }

    pub async fn connect_with_max_frame_bytes(
        path: &Path,
        max_frame_bytes: u64,
    ) -> Result<Self, WireIoError> {
        validate_frame_limit(max_frame_bytes)?;
        let stream =
            UnixStream::connect(path)
                .await
                .map_err(|source| WireIoError::Unreachable {
                    path: path.to_owned(),
                    source,
                })?;
        let (reader, writer) = stream.into_split();
        let state = Arc::new(StdMutex::new(ClientState::default()));
        tokio::spawn(read_responses(
            BufReader::new(reader),
            Arc::clone(&state),
            max_frame_bytes,
        ));
        Ok(Self {
            writer: Arc::new(Mutex::new(writer)),
            state,
            next_id: Arc::new(AtomicU64::new(1)),
            max_frame_bytes,
        })
    }

    pub async fn call(&self, method: &str, params: Option<Value>) -> Result<Value, WireIoError> {
        let next_id = self
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .map_err(|_| {
                WireIoError::InvalidResponse("RPC request ID counter overflowed".to_owned())
            })?;
        let id = RequestId::String(format!("cli-{next_id}"));
        let request = RequestFrame {
            id: id.clone(),
            method: method.to_owned(),
            params,
        };
        let mut encoded = serde_json::to_vec(&request)?;
        ensure_frame_size(encoded.len(), self.max_frame_bytes)?;
        encoded.push(b'\n');

        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.state.lock().expect("RPC client state lock poisoned");
            if let Some(failure) = state.failure.clone() {
                return Err(failure.into_wire_error());
            }
            state.pending.insert(id.clone(), sender);
        }

        if let Err(error) = self.writer.lock().await.write_all(&encoded).await {
            self.state
                .lock()
                .expect("RPC client state lock poisoned")
                .pending
                .remove(&id);
            return Err(WireIoError::Io(error));
        }

        let response = receiver
            .await
            .map_err(|_| WireIoError::Closed)?
            .map_err(ClientReadFailure::into_wire_error)?;
        let object = response
            .as_object()
            .ok_or_else(|| WireIoError::InvalidResponse("response is not an object".to_owned()))?;
        if let Some(result) = object.get("result") {
            return Ok(result.clone());
        }
        if let Some(error) = object.get("error") {
            let error: WireError = serde_json::from_value(error.clone())?;
            return Err(WireIoError::Rpc(error.code, error.message, error.data));
        }
        Err(WireIoError::InvalidResponse(
            "response has neither result nor error".to_owned(),
        ))
    }
}

async fn read_responses(
    mut reader: BufReader<OwnedReadHalf>,
    state: Arc<StdMutex<ClientState>>,
    max_frame_bytes: u64,
) {
    loop {
        let line = match read_line_limited(&mut reader, max_frame_bytes).await {
            Ok(Some(line)) => line,
            Ok(None) => {
                fail_client(&state, ClientReadFailure::Closed);
                return;
            }
            Err(WireIoError::FrameTooLarge { limit }) => {
                fail_client(&state, ClientReadFailure::FrameTooLarge { limit });
                return;
            }
            Err(error) => {
                fail_client(&state, ClientReadFailure::Io(error.to_string()));
                return;
            }
        };
        let response: Value = match serde_json::from_slice(&line) {
            Ok(response) => response,
            Err(error) => {
                fail_client(&state, ClientReadFailure::Json(error.to_string()));
                return;
            }
        };
        let response_id = response
            .as_object()
            .and_then(|object| object.get("id"))
            .cloned()
            .ok_or_else(|| "response has no id".to_owned())
            .and_then(|id| {
                serde_json::from_value::<RequestId>(id)
                    .map_err(|error| format!("response has an invalid id: {error}"))
            });
        let response_id = match response_id {
            Ok(response_id) => response_id,
            Err(error) => {
                fail_client(&state, ClientReadFailure::InvalidResponse(error));
                return;
            }
        };
        let sender = state
            .lock()
            .expect("RPC client state lock poisoned")
            .pending
            .remove(&response_id);
        let Some(sender) = sender else {
            fail_client(
                &state,
                ClientReadFailure::InvalidResponse(format!(
                    "response id {response_id:?} has no pending request"
                )),
            );
            return;
        };
        let _ = sender.send(Ok(response));
    }
}

fn fail_client(state: &Arc<StdMutex<ClientState>>, failure: ClientReadFailure) {
    let pending = {
        let mut state = state.lock().expect("RPC client state lock poisoned");
        if state.failure.is_none() {
            state.failure = Some(failure.clone());
        }
        std::mem::take(&mut state.pending)
    };
    for (_, sender) in pending {
        let _ = sender.send(Err(failure.clone()));
    }
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnqueuePayload {
    #[serde(default)]
    pub invocation: Option<String>,
    #[serde(default)]
    pub argv: Option<Vec<String>>,
    #[serde(
        default,
        rename = "pool",
        serialize_with = "crate::poolset::serialize_optional",
        deserialize_with = "crate::poolset::deserialize_optional"
    )]
    pub pools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    #[serde(default)]
    pub priority: Option<Priority>,
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_options: Option<AdapterJobOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_manifest: Option<GateManifestSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brief_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_from: Option<String>,
    #[serde(default)]
    pub source: Option<EnqueueSource>,
    #[serde(default)]
    pub dedup_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission: Option<SubmissionOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration: Option<Orchestration>,
    #[serde(default)]
    pub parent: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<AdmissionOrigin>,
    #[serde(default)]
    pub caller_job_id: Option<String>,
    #[serde(default, rename = "ghTriggerActor", alias = "ghActor")]
    pub gh_trigger_actor: Option<String>,
    #[serde(default)]
    pub gh_self_actor: Option<String>,
    #[serde(default)]
    pub gh_origin: Option<GhOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub related_trigger: Option<RelatedTrigger>,
    #[serde(default)]
    pub wait: bool,
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
            taskchampion::Uuid::parse_str(resume_from)
                .map_err(|_| WireError::invalid("resumeFrom must be a task UUID"))?;
            if payload.task_uuid.is_some() {
                return Err(WireError::invalid(
                    "resumeFrom and a preassigned taskUuid are mutually exclusive",
                ));
            }
        }
        validate_credentials(&payload.credentials)?;
        let evidence = parse_evidence_specs(&payload.evidence)
            .map_err(|error| WireError::invalid(error.to_string()))?
            .render();

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
            let origin = payload.gh_origin.as_ref().ok_or_else(|| {
                WireError::invalid("taskUuid may be preassigned only by a GitHub trigger")
            })?;
            let expected = gh_trigger_task_uuid(origin)
                .map_err(|error| WireError::invalid(error.to_string()))?
                .to_string();
            if task_uuid != &expected {
                return Err(WireError::invalid(
                    "preassigned GitHub taskUuid does not match its trigger identity",
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
        let adapter = payload.adapter.unwrap_or_else(|| defaults.adapter.clone());
        if adapter.trim().is_empty() {
            return Err(WireError::invalid("adapter must not be empty"));
        }

        let full_submission = payload
            .submission
            .as_ref()
            .is_some_and(|submission| submission.mode == SubmissionMode::Full);
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

    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixListener;
    use tokio::sync::{mpsc, Semaphore};

    use super::*;

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
    }

    impl RpcHandler for MultiplexHandler {
        fn handle<'a>(
            &'a self,
            request: RequestFrame,
        ) -> Pin<Box<dyn Future<Output = Result<Value, WireError>> + 'a>> {
            Box::pin(async move {
                if request.method == "query.status" {
                    return Ok(serde_json::json!({"interleaved": true}));
                }
                let index = request
                    .params
                    .as_ref()
                    .and_then(|params| params["index"].as_u64())
                    .ok_or_else(|| WireError::invalid("missing index"))?;
                self.started.send(index).unwrap();
                tokio::time::sleep(Duration::from_millis((7 - index) * 25)).await;
                Ok(serde_json::json!({"index": index}))
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_connection_multiplexes_six_awaits_and_an_interleaved_query() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let server = tokio::task::spawn_local(async move {
                    let (stream, _) = listener.accept().await.unwrap();
                    serve_connection(
                        stream,
                        MultiplexHandler {
                            started: started_tx,
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
                let status = tokio::time::timeout(
                    Duration::from_millis(40),
                    client.call("query.status", Some(serde_json::json!({}))),
                )
                .await
                .expect("a query must not queue behind blocked awaits")
                .unwrap();
                assert_eq!(status, serde_json::json!({"interleaved": true}));
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

    #[tokio::test(flavor = "current_thread")]
    async fn per_connection_in_flight_window_is_64_and_queues_excess_in_arrival_order() {
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
    async fn default_16_mib_frame_boundary_is_symmetric() {
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
            evidence_class: None,
            manifest_hash: None,
            consumption_estimate: None,
            runtime_max_sec: None,
            no_enqueue: false,
            credentials: BTreeMap::new(),
            origin: None,
            caller_job_id: Some("job-parent".to_owned()),
            gh_trigger_actor: None,
            gh_self_actor: None,
            gh_origin: None,
            task_uuid: None,
            related_trigger: None,
            wait: false,
        }
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
