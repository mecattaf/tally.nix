use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tally_client::{RpcClient, WireErrorCode, WireIoError};
use tally_core::query::QUERY_PROTOCOL_VERSION;
use tally_core::taskdb::RelatedTrigger;
use tally_flow::{
    Admission, ClientError, Disposition, FlowClient, FlowFuture, FlowSubmission, NodeFailure,
    NodeResult, RunInspection, Verdict,
};
use tokio::sync::Mutex;

const RECONNECT_DELAY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunnerIdentity {
    pub(crate) task_uuid: Option<String>,
    pub(crate) job_id: Option<String>,
    pub(crate) related_trigger: Option<RelatedTrigger>,
}

#[derive(Default)]
struct ConnectionState {
    client: Option<RpcClient>,
    generation: u64,
    ever_connected: bool,
}

/// The live FS-5 binding for the deterministic flow engine.
///
/// One `RpcClient` owns one Unix socket and multiplexes every concurrent request by
/// request ID. A broken connection is replaced as a unit, so every outstanding waiter
/// reissues its idempotent operation through the same replacement connection.
pub(crate) struct LiveFlowClient {
    socket: PathBuf,
    max_frame_bytes: u64,
    runner: Mutex<RunnerIdentity>,
    connection: Mutex<ConnectionState>,
}

impl LiveFlowClient {
    pub(crate) fn new(
        socket: impl Into<PathBuf>,
        max_frame_bytes: u64,
        runner: RunnerIdentity,
    ) -> Self {
        Self {
            socket: socket.into(),
            max_frame_bytes,
            runner: Mutex::new(runner),
            connection: Mutex::new(ConnectionState::default()),
        }
    }

    async fn resolve_runner_related_trigger(&self) -> Result<(), ClientError> {
        let task_uuid = self.runner.lock().await.task_uuid.clone();
        let Some(task_uuid) = task_uuid else {
            return Ok(());
        };
        let response = self
            .call("query.job", json!({"id": task_uuid}))
            .await
            .map_err(client_error)?;
        let related_trigger = parse_runner_related_trigger(&response, &task_uuid)?;
        self.runner.lock().await.related_trigger = related_trigger;
        Ok(())
    }

    async fn connection(&self) -> Result<(u64, RpcClient), WireIoError> {
        let mut state = self.connection.lock().await;
        loop {
            if let Some(client) = &state.client {
                return Ok((state.generation, client.clone()));
            }
            match RpcClient::connect_with_max_frame_bytes(&self.socket, self.max_frame_bytes).await
            {
                Ok(client) => {
                    state.generation = state.generation.checked_add(1).ok_or_else(|| {
                        WireIoError::InvalidResponse(
                            "flow RPC connection generation overflowed".to_owned(),
                        )
                    })?;
                    state.ever_connected = true;
                    state.client = Some(client.clone());
                    return Ok((state.generation, client));
                }
                Err(_error) if state.ever_connected => {
                    tokio::time::sleep(RECONNECT_DELAY).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn invalidate(&self, generation: u64) {
        let mut state = self.connection.lock().await;
        if state.generation == generation {
            state.client = None;
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, WireIoError> {
        loop {
            let (generation, client) = self.connection().await?;
            match client.call(method, Some(params.clone())).await {
                Ok(value) => return Ok(value),
                Err(error) if reconnectable(method, &error) => {
                    self.invalidate(generation).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl FlowClient for LiveFlowClient {
    fn inspect_run<'a>(
        &'a self,
        flow_run_id: &'a str,
    ) -> FlowFuture<'a, Result<RunInspection, ClientError>> {
        Box::pin(async move {
            self.resolve_runner_related_trigger().await?;
            'snapshot: loop {
                let mut cursor: Option<String> = None;
                let mut hashes = BTreeSet::new();
                loop {
                    let mut params = json!({"flowRun": flow_run_id, "limit": 1000});
                    if let Some(cursor) = cursor.as_deref() {
                        params["cursor"] = Value::String(cursor.to_owned());
                    }
                    let response = match self.call("query.jobs", params).await {
                        Ok(response) => response,
                        Err(WireIoError::Rpc(WireErrorCode::InvalidParams, message, _))
                            if cursor.is_some() && message.contains("pagination cursor") =>
                        {
                            // Page snapshots belong to a daemon epoch. A restart during
                            // inspection restarts the scan instead of mixing two epochs.
                            continue 'snapshot;
                        }
                        Err(error) => return Err(client_error(error)),
                    };
                    require_query_envelope(&response)?;
                    let items =
                        response
                            .get("items")
                            .and_then(Value::as_array)
                            .ok_or_else(|| {
                                protocol_error("query.jobs response omitted its items array")
                            })?;
                    for item in items {
                        let orchestration = item
                            .get("orchestration")
                            .and_then(Value::as_object)
                            .ok_or_else(|| {
                                protocol_error(
                                    "a query.jobs --flow-run item omitted orchestration provenance",
                                )
                            })?;
                        let hash = orchestration
                            .get("scriptHash")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                protocol_error(
                                    "a query.jobs --flow-run item omitted orchestration.scriptHash",
                                )
                            })?;
                        hashes.insert(hash.to_owned());
                    }
                    cursor = response
                        .get("nextCursor")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    if cursor.is_none() {
                        break;
                    }
                }
                if hashes.len() > 1 {
                    return Err(ClientError::new(
                        "script-history-conflict",
                        format!(
                            "flow run {flow_run_id} contains more than one recorded scriptHash"
                        ),
                    )
                    .with_details(json!({"flowRunId": flow_run_id, "scriptHashes": hashes})));
                }
                return Ok(RunInspection {
                    script_hash: hashes.into_iter().next(),
                });
            }
        })
    }

    fn submit<'a>(
        &'a self,
        submission: FlowSubmission,
    ) -> FlowFuture<'a, Result<Admission, ClientError>> {
        Box::pin(async move {
            let runner = self.runner.lock().await.clone();
            let payload = enqueue_payload(&submission, &runner)?;
            match self.call("queue.enqueue", payload).await {
                Ok(response) => {
                    validate_recorded_script_identity(&response, &submission)?;
                    parse_admission(&response)
                }
                Err(error) => Err(submission_error(&submission, error)),
            }
        })
    }

    fn await_terminal<'a>(
        &'a self,
        task_uuid: &'a str,
        attempt: u32,
    ) -> FlowFuture<'a, Result<NodeResult, ClientError>> {
        Box::pin(async move {
            let response = self
                .call(
                    "queue.await_job",
                    json!({"task_uuid": task_uuid, "attempt": attempt}),
                )
                .await
                .map_err(client_error)?;
            parse_node_result(&response, Disposition::Created)
        })
    }
}

fn reconnectable(method: &str, error: &WireIoError) -> bool {
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

fn enqueue_payload(
    submission: &FlowSubmission,
    runner: &RunnerIdentity,
) -> Result<Value, ClientError> {
    if submission.mode != "full" {
        return Err(ClientError::new(
            "flow-protocol-invalid",
            format!(
                "flow submissions must use full mode, got {:?}",
                submission.mode
            ),
        ));
    }
    let spec = &submission.spec;
    let mut payload = Map::new();
    payload.insert(
        "argv".to_owned(),
        serde_json::to_value(spec.argv.as_ref().ok_or_else(|| {
            ClientError::new("flow-protocol-invalid", "normalized flow node omitted argv")
        })?)
        .map_err(json_client_error)?,
    );
    payload.insert("pool".to_owned(), json!(spec.pools));
    payload.insert(
        "adapter".to_owned(),
        Value::String(spec.adapter.clone().unwrap_or_else(|| "shell".to_owned())),
    );
    payload.insert(
        "source".to_owned(),
        Value::String("orchestrator".to_owned()),
    );
    payload.insert(
        "dedupKey".to_owned(),
        Value::String(submission.dedup_key.clone()),
    );
    payload.insert("submission".to_owned(), json!({"mode": "full"}));
    payload.insert(
        "orchestration".to_owned(),
        serde_json::to_value(&submission.orchestration).map_err(json_client_error)?,
    );
    payload.insert("evidence".to_owned(), json!(spec.evidence));
    payload.insert("noEnqueue".to_owned(), Value::Bool(true));
    payload.insert("wait".to_owned(), Value::Bool(false));
    payload.insert(
        "credentials".to_owned(),
        serde_json::to_value(&submission.credentials).map_err(json_client_error)?,
    );

    insert_optional_string(&mut payload, "executor", spec.executor.as_deref());
    insert_optional_string(&mut payload, "priority", spec.priority.as_deref());
    insert_optional_value(&mut payload, "workspace", spec.workspace.as_ref());
    insert_optional_value(
        &mut payload,
        "adapterOptions",
        spec.adapter_options.as_ref(),
    );
    insert_optional_value(&mut payload, "brief", spec.brief.as_ref());
    insert_optional_value(&mut payload, "evidenceClass", spec.evidence_class.as_ref());
    insert_optional_string(&mut payload, "manifestHash", spec.manifest_hash.as_deref());
    if let Some(runtime_max_sec) = spec.runtime_max_sec {
        payload.insert("runtimeMaxSec".to_owned(), json!(runtime_max_sec));
    }
    if let Some(task_uuid) = runner.task_uuid.as_deref() {
        payload.insert("parent".to_owned(), Value::String(task_uuid.to_owned()));
        if let Some(job_id) = runner.job_id.as_deref() {
            payload.insert("callerJobId".to_owned(), Value::String(job_id.to_owned()));
        }
    }
    if let Some(related_trigger) = &runner.related_trigger {
        payload.insert(
            "relatedTrigger".to_owned(),
            serde_json::to_value(related_trigger).map_err(json_client_error)?,
        );
    }
    Ok(Value::Object(payload))
}

fn insert_optional_string(payload: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        payload.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn insert_optional_value(payload: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        payload.insert(key.to_owned(), value.clone());
    }
}

fn parse_admission(value: &Value) -> Result<Admission, ClientError> {
    let schema_version = required_u32(value, &["schemaVersion"])?;
    let disposition = parse_disposition(required_str(value, &["disposition"])?)?;
    let task_uuid = required_str(value, &["taskUuid", "task_uuid"])?.to_owned();
    let payload_hash = required_str(value, &["payloadHash"])?.to_owned();
    let attempt = required_u32(value, &["attempt"])?;
    if attempt == 0 {
        return Err(protocol_error(
            "full-mode enqueue response attempt must be positive",
        ));
    }
    let terminal = if matches!(disposition, Disposition::Reused | Disposition::Terminal) {
        Some(parse_node_result(value, disposition)?)
    } else {
        None
    };
    Ok(Admission {
        schema_version,
        disposition,
        task_uuid,
        payload_hash,
        attempt,
        terminal,
        recorded_label: optional_str(value, &["recordedLabel"]).map(ToOwned::to_owned),
        reused_rejected: optional_str(value, &["reusedRejected"]).map(ToOwned::to_owned),
    })
}

fn validate_recorded_script_identity(
    value: &Value,
    submission: &FlowSubmission,
) -> Result<(), ClientError> {
    let disposition = parse_disposition(required_str(value, &["disposition"])?)?;
    if disposition == Disposition::Created {
        return Ok(());
    }
    let Some(recorded) = value
        .get("recordedOrchestration")
        .and_then(Value::as_object)
    else {
        let current_prefix = format!("flow:{}:", submission.orchestration.flow_run_id);
        if submission.dedup_key.starts_with(&current_prefix) {
            return Err(protocol_error(format!(
                "{disposition:?} admission for a run-scoped key omitted recordedOrchestration"
            )));
        }
        return Ok(());
    };
    if recorded.get("flowRunId").and_then(Value::as_str)
        != Some(submission.orchestration.flow_run_id.as_str())
    {
        // Explicit author dedup keys may intentionally reuse work across runs.
        return Ok(());
    }
    let recorded_hash = recorded
        .get("scriptHash")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            protocol_error("same-run replay admission omitted recordedOrchestration.scriptHash")
        })?;
    if recorded_hash == submission.orchestration.script_hash {
        return Ok(());
    }
    Err(ClientError::new(
        "script-changed-mid-run",
        format!(
            "flow run {} is pinned to {recorded_hash}, not {}",
            submission.orchestration.flow_run_id, submission.orchestration.script_hash
        ),
    )
    .with_details(json!({
        "flowRunId": submission.orchestration.flow_run_id,
        "recordedHash": recorded_hash,
        "currentHash": submission.orchestration.script_hash,
    })))
}

fn parse_node_result(value: &Value, disposition: Disposition) -> Result<NodeResult, ClientError> {
    let task_uuid = required_str(value, &["taskUuid", "task_uuid"])?.to_owned();
    let verdict = parse_verdict(required_str(value, &["verdict"])?)?;
    let exit_code = optional_i32(value, &["exitCode", "exit_code"])?;
    let witness_seq = required_u64(value, &["witnessSeq", "witness_seq", "witness_lsn"])?;
    let completion = value.get("completion");
    let gates = value
        .get("gates")
        .cloned()
        .or_else(|| completion.and_then(|item| item.get("gates")).cloned());
    let result = projected_result(value);
    let error = value
        .get("error")
        .cloned()
        .map(serde_json::from_value::<NodeFailure>)
        .transpose()
        .map_err(|error| {
            protocol_error(format!(
                "terminal result has an invalid error object: {error}"
            ))
        })?;
    Ok(NodeResult {
        task_uuid,
        verdict,
        exit_code,
        witness_seq,
        disposition,
        result,
        gates,
        error,
    })
}

fn projected_result(value: &Value) -> Option<Value> {
    value
        .get("result")
        .cloned()
        .or_else(|| value.get("finalMessage").cloned())
        .map(|result| match result {
            Value::String(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
            result => result,
        })
}

fn parse_disposition(value: &str) -> Result<Disposition, ClientError> {
    match value {
        "created" => Ok(Disposition::Created),
        "attached" => Ok(Disposition::Attached),
        "reused" => Ok(Disposition::Reused),
        "terminal" => Ok(Disposition::Terminal),
        other => Err(protocol_error(format!(
            "unknown enqueue disposition {other:?}"
        ))),
    }
}

fn parse_verdict(value: &str) -> Result<Verdict, ClientError> {
    match value {
        "pass" => Ok(Verdict::Pass),
        "clean-exit-no-artifact" => Ok(Verdict::CleanExitNoArtifact),
        "failed" => Ok(Verdict::Failed),
        "skipped" => Ok(Verdict::Skipped),
        "cancelled" => Ok(Verdict::Cancelled),
        "pool-vanished" => Ok(Verdict::PoolVanished),
        "preempted" => Ok(Verdict::Preempted),
        "runtime-exceeded" => Ok(Verdict::RuntimeExceeded),
        other => Err(protocol_error(format!(
            "unknown terminal verdict {other:?}"
        ))),
    }
}

fn submission_error(submission: &FlowSubmission, error: WireIoError) -> ClientError {
    if let WireIoError::Rpc(WireErrorCode::DedupKeyConflict, message, Some(data)) = &error {
        if let Some(recorded) = matching_replay_candidate(data, submission) {
            if let Some(recorded_hash) = recorded
                .get("orchestration")
                .and_then(Value::as_object)
                .and_then(|orchestration| orchestration.get("scriptHash"))
                .and_then(Value::as_str)
                .filter(|hash| *hash != submission.orchestration.script_hash)
            {
                return ClientError::new(
                    "script-changed-mid-run",
                    format!(
                        "flow run {} is pinned to {recorded_hash}, not {}",
                        submission.orchestration.flow_run_id, submission.orchestration.script_hash
                    ),
                )
                .with_details(json!({
                    "flowRunId": submission.orchestration.flow_run_id,
                    "recordedHash": recorded_hash,
                    "currentHash": submission.orchestration.script_hash,
                }));
            }
            let recorded_hash = recorded
                .get("payloadHash")
                .and_then(Value::as_str)
                .unwrap_or("<unrecorded>");
            let recorded_label = recorded
                .get("nodeLabel")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return ClientError::new(
                "replay-divergence",
                format!(
                    "ordinal {} re-derived payload {} but the ledger recorded {}",
                    submission.orchestration.node_ordinal, submission.payload_hash, recorded_hash
                ),
            )
            .with_details(json!({
                "expectedHash": submission.payload_hash,
                "recordedHash": recorded_hash,
                "expectedLabel": submission.spec.label,
                "recordedLabel": recorded_label,
                "taskUuid": recorded.get("taskUuid").cloned().unwrap_or(Value::Null),
                "kernelError": message,
            }));
        }
    }
    client_error(error)
}

fn matching_replay_candidate<'a>(
    data: &'a Value,
    submission: &FlowSubmission,
) -> Option<&'a Map<String, Value>> {
    let [candidate] = data.get("existing").and_then(Value::as_array)?.as_slice() else {
        return None;
    };
    let candidate = candidate.as_object()?;
    candidate
        .get("orchestration")
        .and_then(Value::as_object)
        .is_some_and(|orchestration| {
            orchestration.get("flowRunId").and_then(Value::as_str)
                == Some(submission.orchestration.flow_run_id.as_str())
                && orchestration.get("nodeOrdinal").and_then(Value::as_u64)
                    == Some(submission.orchestration.node_ordinal)
        })
        .then_some(candidate)
}

fn client_error(error: WireIoError) -> ClientError {
    match error {
        WireIoError::Rpc(code, message, data) => {
            let stable_code = match code {
                WireErrorCode::DedupKeyConflict => "dedup-key-conflict",
                WireErrorCode::FlowNodeCap => "flow-node-cap",
                WireErrorCode::InvalidParams | WireErrorCode::NotFound => "admission-denied",
                WireErrorCode::FrameTooLarge => "frame-too-large",
                WireErrorCode::UnsupportedProtocol => "unsupported-protocol",
                WireErrorCode::UnknownMethod | WireErrorCode::Unsupported => {
                    "flow-protocol-unavailable"
                }
                WireErrorCode::Timeout => "daemon-timeout",
                WireErrorCode::EpochChanged => "daemon-epoch-changed",
                WireErrorCode::InvalidFrame | WireErrorCode::Internal => "daemon-protocol-error",
            };
            let error = ClientError::new(stable_code, message);
            data.map_or(error.clone(), |details| error.with_details(details))
        }
        other => ClientError::new("daemon-unreachable", other.to_string()),
    }
}

fn parse_runner_related_trigger(
    value: &Value,
    task_uuid: &str,
) -> Result<Option<RelatedTrigger>, ClientError> {
    require_query_envelope(value)?;
    let job = value
        .get("job")
        .and_then(Value::as_object)
        .ok_or_else(|| protocol_error("query.job response omitted its job object"))?;
    if job.get("taskUuid").and_then(Value::as_str) != Some(task_uuid) {
        return Err(protocol_error(
            "query.job response did not identify the running flow parent",
        ));
    }
    let source = job
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| protocol_error("query.job response omitted the parent source"))?;
    // A fallback reference on another non-GitHub row is not authority to relay
    // that reference again. Only the directly triggered GitHub parent threads it.
    if source != "gh" {
        return Ok(None);
    }
    let related_trigger = job
        .get("relatedTrigger")
        .filter(|value| !value.is_null())
        .cloned()
        .ok_or_else(|| {
            protocol_error("GitHub flow parent query omitted its trigger receipt reference")
        })?;
    let related_trigger =
        serde_json::from_value::<RelatedTrigger>(related_trigger).map_err(|error| {
            protocol_error(format!(
                "query.job returned an invalid relatedTrigger: {error}"
            ))
        })?;
    related_trigger.validate().map_err(|error| {
        protocol_error(format!(
            "query.job returned an invalid relatedTrigger: {error}"
        ))
    })?;
    Ok(Some(related_trigger))
}

fn require_query_envelope(value: &Value) -> Result<(), ClientError> {
    if required_u32(value, &["schemaVersion"])? != 1 {
        return Err(protocol_error(
            "flow query returned an unsupported schemaVersion",
        ));
    }
    if required_u32(value, &["protocolVersion"])? != QUERY_PROTOCOL_VERSION {
        return Err(protocol_error(format!(
            "flow queries require protocolVersion {QUERY_PROTOCOL_VERSION}"
        )));
    }
    Ok(())
}

fn required_str<'a>(value: &'a Value, names: &[&str]) -> Result<&'a str, ClientError> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
        .ok_or_else(|| protocol_error(format!("response omitted string field {}", names[0])))
}

fn optional_str<'a>(value: &'a Value, names: &[&str]) -> Option<&'a str> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
}

fn required_u32(value: &Value, names: &[&str]) -> Result<u32, ClientError> {
    let value = required_u64(value, names)?;
    u32::try_from(value)
        .map_err(|_| protocol_error(format!("response field {} exceeds u32", names[0])))
}

fn required_u64(value: &Value, names: &[&str]) -> Result<u64, ClientError> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_u64))
        .ok_or_else(|| protocol_error(format!("response omitted integer field {}", names[0])))
}

fn optional_i32(value: &Value, names: &[&str]) -> Result<Option<i32>, ClientError> {
    let Some(value) = names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_i64))
    else {
        return Ok(None);
    };
    i32::try_from(value)
        .map(Some)
        .map_err(|_| protocol_error(format!("response field {} exceeds i32", names[0])))
}

fn protocol_error(message: impl Into<String>) -> ClientError {
    ClientError::new("flow-protocol-invalid", message)
}

fn json_client_error(error: serde_json::Error) -> ClientError {
    protocol_error(format!("cannot serialize live flow request: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::Path;
    use tally_core::adapters::AdapterJobOptions;
    use tally_core::config::Priority;
    use tally_core::taskdb::{EnqueueSource, RelatedTriggerOutcome};
    use tally_core::wire::{
        canonical_payload, canonical_payload_hash, EnqueuePayload, GuardrailConfig, GuardrailState,
        ProducerDefaults,
    };
    use tally_flow::{NodeSpec, Orchestration};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    fn submission() -> FlowSubmission {
        FlowSubmission {
            mode: "full".to_owned(),
            dedup_key: "flow:00000000-0000-4000-8000-000000000047:0".to_owned(),
            payload_hash: "sha256:expected".to_owned(),
            credentials: BTreeMap::new(),
            spec: NodeSpec {
                argv: Some(vec!["true".to_owned()]),
                adapter: Some("shell".to_owned()),
                prompt: None,
                pools: vec!["slot".to_owned()],
                executor: None,
                priority: Some("low".to_owned()),
                runtime_max_sec: None,
                evidence: vec!["exit:0".to_owned()],
                evidence_class: None,
                manifest_hash: None,
                workspace: None,
                brief: None,
                key: None,
                dedup_key: None,
                label: Some("first".to_owned()),
                env: Default::default(),
                result_schema: None,
                adapter_options: Some(json!({
                    "prePromptArgv": [],
                    "environment": {"SAFE": "yes"}
                })),
                selection: None,
            },
            orchestration: Orchestration {
                flow_name: "fixture".to_owned(),
                flow_run_id: "00000000-0000-4000-8000-000000000047".to_owned(),
                script_hash: "sha256:script".to_owned(),
                node_ordinal: 0,
                node_label: Some("first".to_owned()),
                max_nodes: 10,
                prompt_revision: None,
                skill_revision: None,
                selection: None,
            },
        }
    }

    fn related_trigger() -> RelatedTrigger {
        RelatedTrigger {
            producer: "github-flow".to_owned(),
            event_id: "comment-61".to_owned(),
            outcome: RelatedTriggerOutcome::NotObserved,
            receipt_id: Some("receipt-61".to_owned()),
        }
    }

    #[test]
    fn live_payload_is_full_mode_orchestrator_work_with_captured_ancestry() {
        let mut submission = submission();
        submission.orchestration.prompt_revision = Some(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
        );
        submission.orchestration.skill_revision = Some("review-agent-v3".to_owned());
        let payload = enqueue_payload(
            &submission,
            &RunnerIdentity {
                task_uuid: Some("00000000-0000-4000-8000-000000000048".to_owned()),
                job_id: Some("00000000-0000-4000-8000-000000000048".to_owned()),
                related_trigger: Some(related_trigger()),
            },
        )
        .unwrap();
        assert_eq!(payload["submission"]["mode"], "full");
        assert_eq!(payload["source"], "orchestrator");
        assert_eq!(payload["noEnqueue"], true);
        assert_eq!(payload["parent"], "00000000-0000-4000-8000-000000000048");
        assert_eq!(
            payload["callerJobId"],
            "00000000-0000-4000-8000-000000000048"
        );
        assert_eq!(payload["orchestration"]["nodeOrdinal"], 0);
        assert_eq!(
            payload["orchestration"]["promptRevision"],
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(payload["orchestration"]["skillRevision"], "review-agent-v3");
        assert_eq!(payload["relatedTrigger"]["eventId"], "comment-61");
        assert_eq!(payload["relatedTrigger"]["outcome"], "not-observed");
        assert_eq!(payload["adapterOptions"]["environment"]["SAFE"], "yes");
        assert!(payload.get("payloadHash").is_none());
        assert!(payload.get("label").is_none());
    }

    #[test]
    fn runner_resolves_query_projected_receipt_and_fails_closed_for_github_without_one() {
        let task_uuid = "00000000-0000-4000-8000-000000000048";
        let projected = json!({
            "schemaVersion": 1,
            "protocolVersion": QUERY_PROTOCOL_VERSION,
            "job": {
                "taskUuid": task_uuid,
                "source": "gh",
                "relatedTrigger": related_trigger()
            }
        });
        assert_eq!(
            parse_runner_related_trigger(&projected, task_uuid).unwrap(),
            Some(related_trigger())
        );

        let manual = json!({
            "schemaVersion": 1,
            "protocolVersion": QUERY_PROTOCOL_VERSION,
            "job": {"taskUuid": task_uuid, "source": "manual"}
        });
        assert_eq!(
            parse_runner_related_trigger(&manual, task_uuid).unwrap(),
            None
        );
        let fallback_only = json!({
            "schemaVersion": 1,
            "protocolVersion": QUERY_PROTOCOL_VERSION,
            "job": {
                "taskUuid": task_uuid,
                "source": "orchestrator",
                "relatedTrigger": related_trigger()
            }
        });
        assert_eq!(
            parse_runner_related_trigger(&fallback_only, task_uuid).unwrap(),
            None
        );

        let missing = json!({
            "schemaVersion": 1,
            "protocolVersion": QUERY_PROTOCOL_VERSION,
            "job": {"taskUuid": task_uuid, "source": "gh"}
        });
        assert!(parse_runner_related_trigger(&missing, task_uuid)
            .unwrap_err()
            .message
            .contains("omitted its trigger receipt"));
    }

    #[test]
    fn normalized_live_payload_bytes_match_the_kernel_hash_contract() {
        let mut submission = submission();
        submission.spec.argv = Some(vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            "exit 0".to_owned(),
        ]);
        submission.spec.pools = vec!["alpha".to_owned()];
        submission.spec.priority = Some("high".to_owned());
        submission.spec.evidence = vec!["exit:0".to_owned()];
        submission.credentials.insert(
            "token".to_owned(),
            PathBuf::from("/run/credentials/alpha-token"),
        );
        submission.spec.adapter_options = Some(json!({
            "prePromptArgv": [],
            "environment": {"FLOW_KIND": "shell-a"}
        }));
        let payload: EnqueuePayload = serde_json::from_value(
            enqueue_payload(&submission, &RunnerIdentity::default()).unwrap(),
        )
        .unwrap();
        let defaults = ProducerDefaults {
            pools: vec!["alpha".to_owned()],
            executor: None,
            priority: Priority::Medium,
            adapter: "shell".to_owned(),
            source: EnqueueSource::Manual,
            cwd: None,
            workspace: None,
            adapter_options: AdapterJobOptions::default(),
        };
        let resolved = GuardrailState::new(GuardrailConfig::default())
            .unwrap()
            .validate_enqueue(payload, &defaults)
            .unwrap();
        assert_eq!(
            String::from_utf8(canonical_payload(&resolved).unwrap()).unwrap(),
            concat!(
                r#"{"argv":["/bin/sh","-c","exit 0"],"pool":"alpha","adapter":"shell","#,
                r#""adapterOptions":{"prePromptArgv":[],"environment":{"FLOW_KIND":"shell-a"}},"#,
                r#""evidence":["exit:0"],"noEnqueue":true,"#,
                r#""credentials":{"token":"/run/credentials/alpha-token"}}"#
            )
        );
        assert_eq!(
            canonical_payload_hash(&resolved).unwrap(),
            "sha256:aa09bfa03f0d0a01e824d28d383c029490bc6adbc6917dee5140355e166acada"
        );
    }

    #[test]
    fn matching_kernel_conflict_becomes_replay_divergence_with_both_labels() {
        let error = WireIoError::Rpc(
            WireErrorCode::DedupKeyConflict,
            "dedup-key-conflict".to_owned(),
            Some(json!({
                "existing": [{
                    "taskUuid": "00000000-0000-4000-8000-000000000049",
                    "payloadHash": "sha256:recorded",
                    "nodeLabel": "recorded-first",
                    "orchestration": {
                        "flowRunId": "00000000-0000-4000-8000-000000000047",
                        "scriptHash": "sha256:script",
                        "nodeOrdinal": 0
                    }
                }]
            })),
        );
        let translated = submission_error(&submission(), error);
        assert_eq!(translated.code, "replay-divergence");
        assert_eq!(
            translated.details.as_ref().unwrap()["expectedHash"],
            "sha256:expected"
        );
        assert_eq!(
            translated.details.as_ref().unwrap()["recordedHash"],
            "sha256:recorded"
        );
        assert_eq!(
            translated.details.as_ref().unwrap()["expectedLabel"],
            "first"
        );
        assert_eq!(
            translated.details.as_ref().unwrap()["recordedLabel"],
            "recorded-first"
        );
    }

    #[test]
    fn conflict_from_a_concurrent_script_generation_prefers_script_identity_failure() {
        let error = WireIoError::Rpc(
            WireErrorCode::DedupKeyConflict,
            "dedup-key-conflict".to_owned(),
            Some(json!({
                "existing": [{
                    "taskUuid": "00000000-0000-4000-8000-000000000049",
                    "payloadHash": "sha256:other-payload",
                    "orchestration": {
                        "flowRunId": "00000000-0000-4000-8000-000000000047",
                        "scriptHash": "sha256:other-script",
                        "nodeOrdinal": 0
                    }
                }]
            })),
        );
        let translated = submission_error(&submission(), error);
        assert_eq!(translated.code, "script-changed-mid-run");
        assert_eq!(
            translated.details.as_ref().unwrap()["recordedHash"],
            "sha256:other-script"
        );
    }

    #[test]
    fn ambiguous_live_candidates_remain_a_kernel_dedup_conflict() {
        let candidate = json!({
            "taskUuid": "00000000-0000-4000-8000-000000000049",
            "payloadHash": "sha256:recorded",
            "orchestration": {
                "flowRunId": "00000000-0000-4000-8000-000000000047",
                "nodeOrdinal": 0
            }
        });
        let error = WireIoError::Rpc(
            WireErrorCode::DedupKeyConflict,
            "more than one live row governs this key".to_owned(),
            Some(json!({"existing": [candidate.clone(), candidate]})),
        );
        assert_eq!(
            submission_error(&submission(), error).code,
            "dedup-key-conflict"
        );
    }

    #[test]
    fn same_run_admission_closes_the_concurrent_script_edit_race() {
        let response = json!({
            "schemaVersion": 1,
            "disposition": "attached",
            "task_uuid": "00000000-0000-4000-8000-000000000049",
            "payloadHash": "sha256:expected",
            "attempt": 1,
            "recordedOrchestration": {
                "flowName": "fixture",
                "flowRunId": "00000000-0000-4000-8000-000000000047",
                "scriptHash": "sha256:other-script",
                "nodeOrdinal": 0,
                "maxNodes": 10
            }
        });
        let error = validate_recorded_script_identity(&response, &submission()).unwrap_err();
        assert_eq!(error.code, "script-changed-mid-run");
        assert_eq!(
            error.details.as_ref().unwrap()["recordedHash"],
            "sha256:other-script"
        );

        let mut cross_run = response;
        cross_run["recordedOrchestration"]["flowRunId"] =
            json!("00000000-0000-4000-8000-000000000099");
        validate_recorded_script_identity(&cross_run, &submission()).unwrap();
    }

    #[test]
    fn terminal_wire_shapes_accept_enqueue_and_await_field_names() {
        let admission = parse_admission(&json!({
            "schemaVersion": 1,
            "disposition": "reused",
            "task_uuid": "00000000-0000-4000-8000-000000000049",
            "payloadHash": "sha256:recorded",
            "attempt": 2,
            "verdict": "pass",
            "exit_code": 0,
            "witnessSeq": 7,
            "recordedLabel": "first",
            "completion": {"gates": {"status": "pass"}}
        }))
        .unwrap();
        let terminal = admission.terminal.unwrap();
        assert_eq!(terminal.witness_seq, 7);
        assert_eq!(terminal.gates.unwrap()["status"], "pass");

        let awaited = parse_node_result(
            &json!({
                "task_uuid": "00000000-0000-4000-8000-000000000049",
                "verdict": "pass",
                "exit_code": 0,
                "witness_seq": 8
            }),
            Disposition::Created,
        )
        .unwrap();
        assert_eq!(awaited.witness_seq, 8);
    }

    #[test]
    fn transport_reconnect_classification_is_fail_closed_for_protocol_corruption() {
        assert!(reconnectable("queue.await_job", &WireIoError::Closed));
        assert!(reconnectable(
            "queue.await_job",
            &WireIoError::Rpc(
                WireErrorCode::Internal,
                "daemon stopped while waiting".to_owned(),
                None
            )
        ));
        assert!(!reconnectable(
            "queue.await_job",
            &WireIoError::InvalidResponse("bad id".to_owned())
        ));
        assert!(!reconnectable(
            "queue.enqueue",
            &WireIoError::FrameTooLarge { limit: 1024 }
        ));
    }

    #[test]
    fn socket_path_is_retained_verbatim() {
        let client = LiveFlowClient::new(
            Path::new("/tmp/tally flow.sock"),
            16 * 1024 * 1024,
            RunnerIdentity::default(),
        );
        assert_eq!(client.socket, Path::new("/tmp/tally flow.sock"));
    }

    #[tokio::test]
    async fn concurrent_waits_are_demultiplexed_over_exactly_one_connection() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let mut requests = Vec::new();
            for _ in 0..2 {
                let line = lines.next_line().await.unwrap().unwrap();
                requests.push(serde_json::from_str::<Value>(&line).unwrap());
            }
            for (witness_seq, request) in requests.into_iter().rev().enumerate() {
                let response = json!({
                    "id": request["id"],
                    "result": {
                        "task_uuid": request["params"]["task_uuid"],
                        "verdict": "pass",
                        "exit_code": 0,
                        "witness_seq": witness_seq + 1
                    }
                });
                let mut encoded = serde_json::to_vec(&response).unwrap();
                encoded.push(b'\n');
                write.write_all(&encoded).await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "the live flow client opened more than one runner connection"
            );
        });
        let client = LiveFlowClient::new(&socket, 16 * 1024 * 1024, RunnerIdentity::default());
        let first_id = "00000000-0000-4000-8000-000000000051";
        let second_id = "00000000-0000-4000-8000-000000000052";
        let (first, second) = tokio::join!(
            client.await_terminal(first_id, 1),
            client.await_terminal(second_id, 1)
        );
        assert_eq!(first.unwrap().task_uuid, first_id);
        assert_eq!(second.unwrap().task_uuid, second_id);
        server.await.unwrap();
    }
}
