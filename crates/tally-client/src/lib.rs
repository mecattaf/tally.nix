//! Reference Rust client for tally's Unix-socket NDJSON-RPC protocol.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{oneshot, Mutex};
use tokio::time::Instant;

/// The protocol's default symmetric request and response frame limit (16 MiB).
pub const DEFAULT_MAX_FRAME_BYTES: u64 = 16 * 1024 * 1024;

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
    #[error("RPC method {method} exceeded its {deadline:?} deadline")]
    DeadlineExceeded { method: String, deadline: Duration },
    #[error(
        "daemon socket {path} did not return within {window:?} while re-arming RPC method {method}"
    )]
    RearmDeadlineExceeded {
        method: String,
        path: PathBuf,
        window: Duration,
    },
    #[error("RPC error {0:?}: {1}")]
    Rpc(WireErrorCode, String, Option<Value>),
}

/// Whether an idempotent RPC may be reissued after replacing its connection.
///
/// Callers still decide which operations are idempotent and how long to retry. This
/// classifier only captures the transport and daemon-restart failures shared by the
/// flow runner and the CLI's `queue.await_job` path.
pub fn is_rearmable_rpc_error(method: &str, error: &WireIoError) -> bool {
    match error {
        WireIoError::Unreachable { .. }
        | WireIoError::Io(_)
        | WireIoError::Closed
        | WireIoError::RequestTask(_) => true,
        WireIoError::Rpc(WireErrorCode::EpochChanged | WireErrorCode::Timeout, _, _) => true,
        WireIoError::Rpc(WireErrorCode::Internal, message, _)
            if method == "queue.await_job" && message.contains("daemon stopped while waiting") =>
        {
            true
        }
        _ => false,
    }
}

const AWAIT_JOB_METHOD: &str = "queue.await_job";
const INITIAL_REARM_DELAY: Duration = Duration::from_millis(50);
const MAX_REARM_DELAY: Duration = Duration::from_secs(2);

/// Await a task across bounded daemon reconnects without resubmitting it.
///
/// The initial client is normally the connection that returned the task UUID
/// from `queue.enqueue`. Only the idempotent `queue.await_job` request is
/// reissued. A successfully armed long-poll has no deadline; `rearm_window`
/// bounds only the time spent replacing a failed connection and reissuing the
/// await after a reconnectable error.
pub async fn await_job_with_rearm(
    initial_client: RpcClient,
    path: &Path,
    task_uuid: &str,
    rearm_window: Duration,
) -> Result<Value, WireIoError> {
    let params = Some(serde_json::json!({"task_uuid": task_uuid}));
    let max_frame_bytes = initial_client.max_frame_bytes;
    let mut client = initial_client;
    let mut rearm_deadline = None;
    let mut retry_delay = Duration::ZERO;

    loop {
        match client.call(AWAIT_JOB_METHOD, params.clone()).await {
            Ok(result) => return Ok(result),
            Err(error) if is_rearmable_rpc_error(AWAIT_JOB_METHOD, &error) => {}
            Err(error) => return Err(error),
        }

        let deadline = *rearm_deadline.get_or_insert_with(|| Instant::now() + rearm_window);
        loop {
            if retry_delay != Duration::ZERO {
                tokio::time::sleep_until(deadline.min(Instant::now() + retry_delay)).await;
            }
            if Instant::now() >= deadline {
                return Err(WireIoError::RearmDeadlineExceeded {
                    method: AWAIT_JOB_METHOD.to_owned(),
                    path: path.to_owned(),
                    window: rearm_window,
                });
            }

            match RpcClient::connect_with_max_frame_bytes(path, max_frame_bytes).await {
                Ok(replacement) => {
                    client = replacement;
                    retry_delay = next_rearm_delay(retry_delay);
                    break;
                }
                Err(error) if is_rearmable_rpc_error(AWAIT_JOB_METHOD, &error) => {
                    retry_delay = next_rearm_delay(retry_delay);
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn next_rearm_delay(current: Duration) -> Duration {
    if current == Duration::ZERO {
        INITIAL_REARM_DELAY
    } else {
        current.saturating_mul(2).min(MAX_REARM_DELAY)
    }
}

/// Errors resolving a client's frame limit from tally's rendered configuration.
#[derive(Debug, Error)]
pub enum FrameLimitError {
    #[error("HOME and XDG_CONFIG_HOME are both unset")]
    ConfigHomeUnavailable,
    #[error("cannot read config {path}: {source}")]
    Read { path: String, source: io::Error },
    #[error("invalid JSON configuration: {0}")]
    Json(#[from] serde_json::Error),
    #[error("maxFrameBytes and agingThresholdSec must both be positive")]
    InvalidMaxFrameBytes,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientConfig {
    #[serde(default = "default_max_frame_bytes")]
    max_frame_bytes: u64,
}

const fn default_max_frame_bytes() -> u64 {
    DEFAULT_MAX_FRAME_BYTES
}

/// Resolve the rendered configuration path used by the tally daemon and its clients.
pub fn default_config_path() -> Result<PathBuf, FrameLimitError> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("tally/config.json"));
    }
    let home = std::env::var_os("HOME").ok_or(FrameLimitError::ConfigHomeUnavailable)?;
    Ok(PathBuf::from(home).join(".config/tally/config.json"))
}

/// Resolve the symmetric wire-frame limit from tally's rendered configuration.
///
/// An explicit path must exist and contain valid JSON. Without an explicit path, an
/// unavailable configuration home or absent default file resolves to the protocol default.
pub fn resolve_max_frame_bytes(config_path: Option<&Path>) -> Result<u64, FrameLimitError> {
    let (path, explicit) = if let Some(path) = config_path {
        (path.to_owned(), true)
    } else {
        let Ok(path) = default_config_path() else {
            return Ok(DEFAULT_MAX_FRAME_BYTES);
        };
        (path, false)
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(source) if !explicit && source.kind() == io::ErrorKind::NotFound => {
            return Ok(DEFAULT_MAX_FRAME_BYTES);
        }
        Err(source) => {
            return Err(FrameLimitError::Read {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let config: ClientConfig = serde_json::from_slice(&bytes)?;
    if config.max_frame_bytes == 0 {
        return Err(FrameLimitError::InvalidMaxFrameBytes);
    }
    Ok(config.max_frame_bytes)
}

/// Low-level framing helpers shared with the daemon-side transport.
#[doc(hidden)]
pub mod framing {
    use tokio::io::AsyncBufReadExt;

    use super::WireIoError;

    pub async fn read_line_limited<R: tokio::io::AsyncBufRead + Unpin>(
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

    pub fn validate_frame_limit(max_frame_bytes: u64) -> Result<(), WireIoError> {
        if max_frame_bytes == 0 {
            Err(WireIoError::InvalidFrameLimit)
        } else {
            Ok(())
        }
    }

    pub fn ensure_frame_size(
        encoded_bytes: usize,
        max_frame_bytes: u64,
    ) -> Result<(), WireIoError> {
        let frame_bytes =
            (encoded_bytes as u64)
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

/// One multiplexed connection to a tally daemon.
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
        framing::validate_frame_limit(max_frame_bytes)?;
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
        self.call_inner(method, params, None).await
    }

    pub async fn call_with_deadline(
        &self,
        method: &str,
        params: Option<Value>,
        deadline: Duration,
    ) -> Result<Value, WireIoError> {
        self.call_inner(method, params, Some(deadline)).await
    }

    async fn call_inner(
        &self,
        method: &str,
        params: Option<Value>,
        deadline: Option<Duration>,
    ) -> Result<Value, WireIoError> {
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
        framing::ensure_frame_size(encoded.len(), self.max_frame_bytes)?;
        encoded.push(b'\n');

        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.state.lock().expect("RPC client state lock poisoned");
            if let Some(failure) = state.failure.clone() {
                return Err(failure.into_wire_error());
            }
            state.pending.insert(id.clone(), sender);
        }

        let exchange = async {
            self.writer
                .lock()
                .await
                .write_all(&encoded)
                .await
                .map_err(WireIoError::Io)?;
            receiver
                .await
                .map_err(|_| WireIoError::Closed)?
                .map_err(ClientReadFailure::into_wire_error)
        };
        let response = if let Some(deadline) = deadline {
            match tokio::time::timeout(deadline, exchange).await {
                Ok(response) => response,
                Err(_) => Err(WireIoError::DeadlineExceeded {
                    method: method.to_owned(),
                    deadline,
                }),
            }
        } else {
            exchange.await
        };
        if response.is_err() {
            self.state
                .lock()
                .expect("RPC client state lock poisoned")
                .pending
                .remove(&id);
        }
        let response = response?;
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
        let line = match framing::read_line_limited(&mut reader, max_frame_bytes).await {
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

#[cfg(test)]
mod tests {
    use proptest::collection::vec;
    use proptest::prelude::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    use super::*;

    proptest! {
        #[test]
        fn arbitrary_ndjson_input_is_bounded_by_the_frame_limit(
            input in vec(any::<u8>(), 0..2_048),
            max_frame_bytes in 1_u64..=512,
            buffer_capacity in 1_usize..=128,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut reader = BufReader::with_capacity(buffer_capacity, input.as_slice());
            let result = runtime.block_on(framing::read_line_limited(
                &mut reader,
                max_frame_bytes,
            ));
            let wire_len = input
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(input.len(), |newline| newline + 1);

            if wire_len as u64 > max_frame_bytes {
                let is_frame_too_large = matches!(
                    result,
                    Err(WireIoError::FrameTooLarge { limit }) if limit == max_frame_bytes
                );
                prop_assert!(is_frame_too_large);
            } else if input.is_empty() {
                prop_assert!(matches!(result, Ok(None)));
            } else {
                let mut expected = input[..wire_len].to_vec();
                let terminated = expected.last() == Some(&b'\n');
                if terminated {
                    expected.pop();
                    if expected.last() == Some(&b'\r') {
                        expected.pop();
                    }
                }
                let line = result.unwrap().unwrap();
                prop_assert_eq!(&line, &expected);
                prop_assert!(line.len() as u64 <= max_frame_bytes);
            }
        }

        #[test]
        fn valid_request_frames_round_trip_byte_for_byte(
            id in any::<i64>(),
            method in "[a-z]{1,16}(\\.[a-z_]{1,16}){0,2}",
            value in any::<i64>(),
            buffer_capacity in 1_usize..=64,
        ) {
            let request = RequestFrame {
                id: RequestId::Number(id),
                method,
                params: Some(serde_json::json!({"value": value})),
            };
            let encoded = serde_json::to_vec(&request).unwrap();
            let mut wire = encoded.clone();
            wire.push(b'\n');
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let mut reader = BufReader::with_capacity(buffer_capacity, wire.as_slice());
            let decoded = runtime
                .block_on(framing::read_line_limited(&mut reader, wire.len() as u64))
                .unwrap()
                .unwrap();

            prop_assert_eq!(&decoded, &encoded);
            prop_assert_eq!(serde_json::from_slice::<RequestFrame>(&decoded).unwrap(), request);
        }
    }

    #[test]
    fn explicit_config_controls_the_transport_limit() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        std::fs::write(
            &config,
            concat!(
                r#"{"maxFrameBytes":20971520,"agingThresholdSec":3600,"pools":{"slot":{"#,
                r#""credentials":{"token":"/run/credentials/slot-token"}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(
            resolve_max_frame_bytes(Some(&config)).unwrap(),
            20 * 1024 * 1024
        );
        assert!(resolve_max_frame_bytes(Some(&temp.path().join("missing.json"))).is_err());
    }

    #[test]
    fn absent_frame_limit_uses_the_protocol_default() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        std::fs::write(&config, "{}").unwrap();
        assert_eq!(
            resolve_max_frame_bytes(Some(&config)).unwrap(),
            DEFAULT_MAX_FRAME_BYTES
        );
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
    fn rearmable_errors_match_the_flow_runner_restart_contract() {
        assert!(is_rearmable_rpc_error(
            "queue.await_job",
            &WireIoError::Closed
        ));
        assert!(is_rearmable_rpc_error(
            "query.job",
            &WireIoError::Io(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        ));
        assert!(is_rearmable_rpc_error(
            "query.job",
            &WireIoError::Rpc(WireErrorCode::EpochChanged, "new epoch".to_owned(), None)
        ));
        assert!(is_rearmable_rpc_error(
            "queue.await_job",
            &WireIoError::Rpc(
                WireErrorCode::Internal,
                "daemon stopped while waiting for terminal state".to_owned(),
                None,
            )
        ));
        assert!(!is_rearmable_rpc_error(
            "query.job",
            &WireIoError::Rpc(
                WireErrorCode::Internal,
                "daemon stopped while waiting for terminal state".to_owned(),
                None,
            )
        ));
        assert!(!is_rearmable_rpc_error(
            "queue.await_job",
            &WireIoError::InvalidResponse("bad response id".to_owned())
        ));
        assert!(!is_rearmable_rpc_error(
            "queue.await_job",
            &WireIoError::RearmDeadlineExceeded {
                method: "queue.await_job".to_owned(),
                path: PathBuf::from("/run/tally.sock"),
                window: Duration::from_secs(60),
            }
        ));
    }

    #[tokio::test]
    async fn deadline_removes_pending_request_and_late_response_fails_cleanly() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: RequestFrame = serde_json::from_str(line.trim_end()).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            let mut response = serde_json::to_vec(&serde_json::json!({
                "id": request.id,
                "result": {"ok": true},
            }))
            .unwrap();
            response.push(b'\n');
            writer.write_all(&response).await.unwrap();
        });

        let client = RpcClient::connect(&socket).await.unwrap();
        let deadline = Duration::from_millis(20);
        let error = client
            .call_with_deadline("query.status", None, deadline)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            WireIoError::DeadlineExceeded {
                ref method,
                deadline: actual,
            } if method == "query.status" && actual == deadline
        ));
        assert!(client
            .state
            .lock()
            .expect("RPC client state lock poisoned")
            .pending
            .is_empty());

        server.await.unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let failure = client
                    .state
                    .lock()
                    .expect("RPC client state lock poisoned")
                    .failure
                    .clone();
                if let Some(failure) = failure {
                    break failure;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("late response should fail the connection cleanly");
        assert!(matches!(
            failure,
            ClientReadFailure::InvalidResponse(message)
                if message.contains("has no pending request")
        ));
    }

    #[tokio::test]
    async fn await_job_rearms_only_the_await_after_a_disconnect() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let replacement_socket = socket.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let first: RequestFrame = serde_json::from_str(line.trim_end()).unwrap();
            assert_eq!(first.method, AWAIT_JOB_METHOD);
            assert_eq!(
                first.params,
                Some(serde_json::json!({"task_uuid": "task-7"}))
            );
            drop(reader);
            drop(listener);
            std::fs::remove_file(&replacement_socket).unwrap();

            let replacement = UnixListener::bind(&replacement_socket).unwrap();
            let (stream, _) = replacement.accept().await.unwrap();
            let (reader, mut writer) = stream.into_split();
            let mut reader = BufReader::new(reader);
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let second: RequestFrame = serde_json::from_str(line.trim_end()).unwrap();
            assert_eq!(second.method, AWAIT_JOB_METHOD);
            assert_eq!(second.params, first.params);
            let mut response = serde_json::to_vec(&serde_json::json!({
                "id": second.id,
                "result": {"task_uuid": "task-7", "verdict": "pass"},
            }))
            .unwrap();
            response.push(b'\n');
            writer.write_all(&response).await.unwrap();
        });

        let client = RpcClient::connect(&socket).await.unwrap();
        let result = await_job_with_rearm(client, &socket, "task-7", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(result["verdict"], "pass");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn await_job_rearm_exhaustion_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let removed_socket = socket.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: RequestFrame = serde_json::from_str(line.trim_end()).unwrap();
            assert_eq!(request.method, AWAIT_JOB_METHOD);
            drop(reader);
            drop(listener);
            std::fs::remove_file(removed_socket).unwrap();
        });

        let client = RpcClient::connect(&socket).await.unwrap();
        let window = Duration::from_millis(120);
        let started = Instant::now();
        let error = await_job_with_rearm(client, &socket, "task-8", window)
            .await
            .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            error,
            WireIoError::RearmDeadlineExceeded {
                ref method,
                ref path,
                window: actual,
            } if method == AWAIT_JOB_METHOD && path == &socket && actual == window
        ));
        server.await.unwrap();
    }
}
