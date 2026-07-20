use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

use crate::config::Priority;
use crate::evidence::parse_evidence_specs;
use crate::taskdb::{EnqueueSource, GhOrigin};

pub const FRAME_CAP_BYTES: usize = 64 * 1024;

pub const RPC_METHODS: &[&str] = &[
    "queue.enqueue",
    "queue.cancel",
    "queue.pause",
    "queue.resume",
    "queue.drain",
    "queue.await_job",
    "queue.await_barrier",
    "lease.acquire",
    "lease.release",
    "lease.status",
    "query.status",
    "query.log",
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
    #[error("wire frame exceeds {FRAME_CAP_BYTES} bytes")]
    FrameTooLarge,
    #[error("daemon closed the socket before replying")]
    Closed,
    #[error("invalid response frame: {0}")]
    InvalidResponse(String),
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
) -> Result<Option<Vec<u8>>, WireIoError> {
    let mut line = Vec::new();
    let read = reader.read_until(b'\n', &mut line).await?;
    if read == 0 {
        return Ok(None);
    }
    if line.len() > FRAME_CAP_BYTES {
        return Err(WireIoError::FrameTooLarge);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    Ok(Some(line))
}

pub async fn serve_connection(
    stream: UnixStream,
    handler: &dyn RpcHandler,
) -> Result<(), WireIoError> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    while let Some(line) = read_line_limited(&mut reader).await? {
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
                write_frame(&mut writer, &response).await?;
                continue;
            }
        };
        let encoded = match handler.handle(request.clone()).await {
            Ok(result) => serde_json::to_value(ResponseOk {
                id: &request.id,
                result,
            })?,
            Err(error) => serde_json::to_value(ResponseErr {
                id: &request.id,
                error,
            })?,
        };
        write_frame(&mut writer, &encoded).await?;
    }
    Ok(())
}

async fn write_frame(writer: &mut OwnedWriteHalf, value: &Value) -> Result<(), WireIoError> {
    let mut encoded = serde_json::to_vec(value)?;
    if encoded.len() + 1 > FRAME_CAP_BYTES {
        return Err(WireIoError::FrameTooLarge);
    }
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    Ok(())
}

pub struct RpcClient {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
    next_id: u64,
}

impl RpcClient {
    pub async fn connect(path: &Path) -> Result<Self, WireIoError> {
        let stream =
            UnixStream::connect(path)
                .await
                .map_err(|source| WireIoError::Unreachable {
                    path: path.to_owned(),
                    source,
                })?;
        let (reader, writer) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        })
    }

    pub async fn call(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, WireIoError> {
        let id = RequestId::String(format!("cli-{}", self.next_id));
        self.next_id += 1;
        let request = RequestFrame {
            id: id.clone(),
            method: method.to_owned(),
            params,
        };
        let mut encoded = serde_json::to_vec(&request)?;
        if encoded.len() + 1 > FRAME_CAP_BYTES {
            return Err(WireIoError::FrameTooLarge);
        }
        encoded.push(b'\n');
        self.writer.write_all(&encoded).await?;

        let line = read_line_limited(&mut self.reader)
            .await?
            .ok_or(WireIoError::Closed)?;
        let response: Value = serde_json::from_slice(&line)?;
        let object = response
            .as_object()
            .ok_or_else(|| WireIoError::InvalidResponse("response is not an object".to_owned()))?;
        let response_id: RequestId = serde_json::from_value(
            object
                .get("id")
                .cloned()
                .ok_or_else(|| WireIoError::InvalidResponse("response has no id".to_owned()))?,
        )?;
        if response_id != id {
            return Err(WireIoError::InvalidResponse(format!(
                "response id {response_id:?} does not match request id {id:?}"
            )));
        }
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
    #[serde(default)]
    pub source: Option<EnqueueSource>,
    #[serde(default)]
    pub dedup_key: Option<String>,
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
    #[serde(default)]
    pub caller_job_id: Option<String>,
    #[serde(default)]
    pub gh_actor: Option<String>,
    #[serde(default)]
    pub gh_self_actor: Option<String>,
    #[serde(default)]
    pub gh_origin: Option<GhOrigin>,
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
    pub source: EnqueueSource,
    pub dedup_key: Option<String>,
    pub parent: Option<String>,
    pub evidence: Vec<String>,
    pub evidence_class: Option<Value>,
    pub manifest_hash: Option<String>,
    pub consumption_estimate: Option<u64>,
    pub runtime_max_sec: Option<u64>,
    pub no_enqueue: bool,
    pub credentials: BTreeMap<String, PathBuf>,
    pub gh_origin: Option<GhOrigin>,
    pub depth: u32,
    pub wait: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentInfo {
    pub parent_uuid: String,
    pub depth: u32,
    pub children: u32,
    pub no_enqueue: bool,
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

    pub fn rollback_child_charge(&mut self, job_id: &str) -> Result<(), WireError> {
        let info = self
            .parents
            .get_mut(job_id)
            .ok_or_else(|| WireError::not_found(format!("unknown parent job {job_id}")))?;
        info.children = info.children.checked_sub(1).ok_or_else(|| {
            WireError::new(
                WireErrorCode::Internal,
                format!("parent job {job_id} has no child charge to roll back"),
            )
        })?;
        Ok(())
    }

    pub fn validate_enqueue(
        &mut self,
        payload: EnqueuePayload,
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
        validate_credentials(&payload.credentials)?;
        let evidence = parse_evidence_specs(&payload.evidence)
            .map_err(|error| WireError::invalid(error.to_string()))?
            .render();

        let source = payload.source.unwrap_or(defaults.source);
        if payload.gh_origin.is_some() && source != EnqueueSource::Gh {
            return Err(WireError::invalid("ghOrigin is valid only for source=gh"));
        }
        if let Some(origin) = &payload.gh_origin {
            origin
                .validate()
                .map_err(|error| WireError::invalid(error.to_string()))?;
            if payload.gh_actor.as_deref() != Some(origin.actor.as_str())
                || payload.gh_self_actor.as_deref() != Some(origin.self_actor.as_str())
            {
                return Err(WireError::invalid(
                    "GitHub actor fields do not match the durable ghOrigin",
                ));
            }
        }
        let excluded = payload.gh_origin.as_ref().is_some_and(|origin| {
            if origin.actor_exclude == "self" {
                origin.actor == origin.self_actor
            } else {
                origin.actor == origin.actor_exclude
            }
        }) || payload.gh_origin.is_none()
            && payload
                .gh_actor
                .as_deref()
                .zip(payload.gh_self_actor.as_deref())
                .is_some_and(|(actor, own)| actor == own);
        if source == EnqueueSource::Gh && excluded {
            return Err(WireError::invalid(
                "GitHub event actor is excluded by actorExclude",
            ));
        }
        let mut pools = payload.pools.unwrap_or_else(|| defaults.pools.clone());
        crate::poolset::canonicalize(&mut pools)
            .map_err(|error| WireError::invalid(error.to_string()))?;
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
            if info.children >= self.config.fanout_cap {
                return Err(WireError::invalid(format!(
                    "parent fanout would exceed fanoutCap {}",
                    self.config.fanout_cap
                )));
            }
            parent = Some(info.parent_uuid.clone());
            info.children += 1;
        }

        Ok(ResolvedEnqueue {
            argv,
            pools,
            executor: payload.executor.or_else(|| defaults.executor.clone()),
            priority: payload.priority.unwrap_or(defaults.priority),
            adapter,
            source,
            dedup_key: payload.dedup_key,
            parent,
            evidence,
            evidence_class: payload.evidence_class,
            manifest_hash: payload.manifest_hash,
            consumption_estimate: payload.consumption_estimate,
            runtime_max_sec: payload.runtime_max_sec,
            no_enqueue: payload.no_enqueue,
            credentials: payload.credentials,
            gh_origin: payload.gh_origin,
            depth,
            wait: payload.wait,
        })
    }
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

    use tokio::net::UnixListener;

    use super::*;

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
                    serve_connection(stream, &EchoHandler).await.unwrap();
                });
                let mut client = RpcClient::connect(&socket).await.unwrap();
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
            source: None,
            dedup_key: Some("child-1".to_owned()),
            parent: None,
            evidence: Vec::new(),
            evidence_class: None,
            manifest_hash: None,
            consumption_estimate: None,
            runtime_max_sec: None,
            no_enqueue: false,
            credentials: BTreeMap::new(),
            caller_job_id: Some("job-parent".to_owned()),
            gh_actor: None,
            gh_self_actor: None,
            gh_origin: None,
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
                children: 0,
                no_enqueue: false,
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
                children: 0,
                no_enqueue: false,
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
                children: 0,
                no_enqueue: false,
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
                children: 0,
                no_enqueue: true,
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
        payload.gh_actor = Some("bot".to_owned());
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
                children: 0,
                no_enqueue: false,
            },
        );

        let mut malformed = child_payload();
        malformed.evidence = vec!["hash:sha256:short".to_owned()];
        assert!(state.validate_enqueue(malformed, &defaults()).is_err());
        assert_eq!(state.parent("job-parent").unwrap().children, 0);

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
        assert_eq!(state.parent("job-parent").unwrap().children, 1);
        state.rollback_child_charge("job-parent").unwrap();
        assert_eq!(state.parent("job-parent").unwrap().children, 0);
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
}
