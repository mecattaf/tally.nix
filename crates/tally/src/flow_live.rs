use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use serde_json::{json, Map, Value};
use tally_client::{is_rearmable_rpc_error, RpcClient, WireErrorCode, WireIoError};
use tally_core::flow_lineage::FLOW_LINEAGE_SCHEMA_VERSION;
use tally_core::query::QUERY_PROTOCOL_VERSION;
use tally_core::taskdb::RelatedTrigger;
use tally_flow::{
    Admission, ClientError, Disposition, FlowClient, FlowFuture, FlowSubmission, LifecycleSink,
    NodeFailure, NodeResult, RunInspection, RunSupersede, TaskRef, Verdict,
};
use tokio::sync::Mutex;
use tokio::time::Instant;

const LIVE_CALL_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const LIVE_RETRY_LIMIT: u32 = 64;
const LIVE_RETRY_BASE_DELAY: Duration = Duration::from_millis(50);
const LIVE_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
const RESULT_PROJECTION_RETRY: Duration = Duration::from_millis(10);
const RESULT_PROJECTION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RunnerIdentity {
    pub(crate) task_uuid: Option<String>,
    pub(crate) job_id: Option<String>,
    pub(crate) job_token: Option<String>,
    pub(crate) related_trigger: Option<RelatedTrigger>,
}

#[derive(Default)]
struct ConnectionState {
    client: Option<RpcClient>,
    generation: u64,
    ever_connected: bool,
}

#[derive(Clone)]
struct ResultProjectionExpectation {
    adapter: String,
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
    final_message_adapters: BTreeSet<String>,
    result_expected: Mutex<BTreeMap<(String, u32), ResultProjectionExpectation>>,
    result_projection_timeout: Duration,
    call_timeout: Duration,
    retry_limit: u32,
    retry_base_delay: Duration,
    lifecycle: Option<Rc<dyn LifecycleSink>>,
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
            final_message_adapters: BTreeSet::new(),
            result_expected: Mutex::new(BTreeMap::new()),
            result_projection_timeout: RESULT_PROJECTION_TIMEOUT,
            call_timeout: LIVE_CALL_TIMEOUT,
            retry_limit: LIVE_RETRY_LIMIT,
            retry_base_delay: LIVE_RETRY_BASE_DELAY,
            lifecycle: None,
        }
    }

    #[must_use]
    pub(crate) fn with_final_message_adapters(
        mut self,
        final_message_adapters: BTreeSet<String>,
    ) -> Self {
        self.final_message_adapters = final_message_adapters;
        self
    }

    #[must_use]
    pub(crate) fn with_call_timeout(mut self, call_timeout: Duration) -> Self {
        self.call_timeout = call_timeout;
        self
    }

    #[must_use]
    pub(crate) fn with_lifecycle_sink(mut self, lifecycle: Rc<dyn LifecycleSink>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
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

    /// Ask the daemon whether this run ID was durably retired.
    ///
    /// The lineage ledger has its own `schemaVersion`, not the paginated query
    /// envelope's, so this reads the record directly.
    async fn inspect_supersede(
        &self,
        flow_run_id: &str,
    ) -> Result<Option<RunSupersede>, ClientError> {
        let response = self
            .call("query.lineage", json!({"flowRun": flow_run_id}))
            .await
            .map_err(client_error)?;
        if required_u32(&response, &["schemaVersion"])? != FLOW_LINEAGE_SCHEMA_VERSION {
            return Err(protocol_error(
                "query.lineage returned an unsupported schemaVersion",
            ));
        }
        let Some(record) = response
            .get("supersededBy")
            .filter(|value| !value.is_null())
        else {
            return Ok(None);
        };
        let field = |name: &str| {
            record
                .get(name)
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .ok_or_else(|| protocol_error(format!("query.lineage supersededBy omitted {name}")))
        };
        Ok(Some(RunSupersede {
            successor_flow_run_id: field("successorFlowRunId")?,
            reason: field("reason")?,
            recorded_at: field("recordedAt")?,
        }))
    }

    async fn connection(&self) -> Result<(u64, RpcClient), WireIoError> {
        let mut state = self.connection.lock().await;
        if let Some(client) = &state.client {
            return Ok((state.generation, client.clone()));
        }
        let client =
            RpcClient::connect_with_max_frame_bytes(&self.socket, self.max_frame_bytes).await?;
        state.generation = state.generation.checked_add(1).ok_or_else(|| {
            WireIoError::InvalidResponse("flow RPC connection generation overflowed".to_owned())
        })?;
        state.ever_connected = true;
        state.client = Some(client.clone());
        Ok((state.generation, client))
    }

    async fn invalidate(&self, generation: u64) {
        let mut state = self.connection.lock().await;
        if state.generation == generation {
            state.client = None;
        }
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, WireIoError> {
        let deadline = Instant::now() + self.call_timeout;
        let mut retries = 0;
        loop {
            let (generation, client) =
                match tokio::time::timeout_at(deadline, self.connection()).await {
                    Ok(Ok(connection)) => connection,
                    Ok(Err(error)) => {
                        if !self.connection.lock().await.ever_connected
                            || !reconnectable(method, &error)
                        {
                            return Err(error);
                        }
                        self.wait_to_retry(method, &error, deadline, &mut retries)
                            .await?;
                        continue;
                    }
                    Err(_) => return Err(call_timeout_error(method, retries)),
                };
            match tokio::time::timeout_at(deadline, client.call(method, Some(params.clone()))).await
            {
                Ok(Ok(value)) => return Ok(value),
                Ok(Err(error)) if reconnectable(method, &error) => {
                    self.invalidate(generation).await;
                    self.wait_to_retry(method, &error, deadline, &mut retries)
                        .await?;
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    self.invalidate(generation).await;
                    return Err(call_timeout_error(method, retries));
                }
            }
        }
    }

    async fn wait_to_retry(
        &self,
        method: &str,
        last_error: &WireIoError,
        deadline: Instant,
        retries: &mut u32,
    ) -> Result<(), WireIoError> {
        if *retries >= self.retry_limit {
            return Err(retry_limit_error(method, self.retry_limit, last_error));
        }
        if *retries == 0 {
            if let Some(lifecycle) = &self.lifecycle {
                lifecycle
                    .emit(json!({
                        "type": "flow-rpc-reconnect",
                        "method": method,
                        "attempt": 1,
                        "error": last_error.to_string(),
                    }))
                    .map_err(|error| WireIoError::ClientCallback(error.to_string()))?;
            }
        }
        *retries += 1;
        if *retries == 1 {
            return Ok(());
        }
        let delay = live_retry_delay(*retries, self.retry_base_delay);
        let now = Instant::now();
        if now >= deadline {
            return Err(call_timeout_error(method, *retries));
        }
        let wake = deadline.min(now + delay);
        tokio::time::sleep_until(wake).await;
        if Instant::now() >= deadline {
            return Err(call_timeout_error(method, *retries));
        }
        Ok(())
    }

    async fn await_projected_result(&self, task_uuid: &str) -> Result<Option<Value>, ClientError> {
        let deadline = Instant::now() + self.result_projection_timeout;
        loop {
            let response = match tokio::time::timeout_at(
                deadline,
                self.call("query.job", json!({"id": task_uuid})),
            )
            .await
            {
                Ok(response) => response.map_err(client_error)?,
                Err(_) => return Ok(None),
            };
            require_query_envelope(&response)?;
            if let Some(message) = response
                .get("job")
                .and_then(|job| job.get("finalMessage"))
                .and_then(|message| message.get("value"))
                .and_then(Value::as_str)
            {
                return Ok(Some(decode_projected_result(message)));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep_until(deadline.min(Instant::now() + RESULT_PROJECTION_RETRY)).await;
        }
    }

    async fn enrich_terminal_result(
        &self,
        result: &mut NodeResult,
        attempt: u32,
    ) -> Result<(), ClientError> {
        let key = (result.task_uuid.clone(), attempt);
        let expectation = self.result_expected.lock().await.get(&key).cloned();
        let projected = if expectation.is_some() && result.result.is_none() {
            self.await_projected_result(&result.task_uuid).await
        } else {
            Ok(None)
        };
        if let Some(projected) = projected? {
            result.result = Some(projected);
        } else if let Some(expectation) = expectation.filter(|_| result.error.is_none()) {
            result.error = Some(NodeFailure {
                code: "result-projection-timeout".to_owned(),
                message: format!(
                    "configured finalMessage capture for adapter {:?} was not projected within {} ms",
                    expectation.adapter,
                    self.result_projection_timeout.as_millis()
                ),
                details: Some(json!({
                    "adapter": expectation.adapter,
                    "attempt": attempt,
                    "taskUuid": result.task_uuid,
                    "timeoutMs": self.result_projection_timeout.as_millis(),
                })),
            });
        }
        Ok(())
    }
}

fn live_retry_delay(retry: u32, base: Duration) -> Duration {
    let multiplier = 1_u32 << retry.saturating_sub(2).min(16);
    base.saturating_mul(multiplier).min(LIVE_RETRY_MAX_DELAY)
}

impl FlowClient for LiveFlowClient {
    fn inspect_run<'a>(
        &'a self,
        flow_run_id: &'a str,
    ) -> FlowFuture<'a, Result<RunInspection, ClientError>> {
        Box::pin(async move {
            self.resolve_runner_related_trigger().await?;
            // Lineage first. A superseded run is terminal by durable decision,
            // and answering that before the row scan means the supervisor is
            // told which run to start rather than which hash moved.
            if let Some(supersede) = self.inspect_supersede(flow_run_id).await? {
                return Ok(RunInspection {
                    supersede: Some(supersede),
                    ..RunInspection::default()
                });
            }
            'snapshot: loop {
                let mut cursor: Option<String> = None;
                let mut script_hashes = BTreeSet::new();
                let mut args_hashes = BTreeSet::new();
                let mut catalog_hashes = BTreeSet::new();
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
                        let script_hash = orchestration
                            .get("scriptHash")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                            protocol_error(
                                "a query.jobs --flow-run item omitted orchestration.scriptHash",
                            )
                        })?;
                        let args_hash = orchestration
                            .get("argsHash")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                protocol_error(
                                    "a query.jobs --flow-run item omitted orchestration.argsHash",
                                )
                            })?;
                        let catalog_hash = match orchestration.get("catalogHash") {
                            Some(Value::String(hash)) => Some(hash.clone()),
                            Some(Value::Null) => None,
                            _ => {
                                return Err(protocol_error(
                                    "a query.jobs --flow-run item omitted orchestration.catalogHash or supplied an invalid value",
                                ));
                            }
                        };
                        script_hashes.insert(script_hash.to_owned());
                        args_hashes.insert(args_hash.to_owned());
                        catalog_hashes.insert(catalog_hash);
                    }
                    cursor = response
                        .get("nextCursor")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    if cursor.is_none() {
                        break;
                    }
                }
                if script_hashes.len() > 1 {
                    return Err(ClientError::new(
                        "script-history-conflict",
                        format!(
                            "flow run {flow_run_id} contains more than one recorded scriptHash"
                        ),
                    )
                    .with_details(json!({
                        "flowRunId": flow_run_id,
                        "scriptHashes": script_hashes,
                    })));
                }
                if args_hashes.len() > 1 {
                    return Err(ClientError::new(
                        "args-history-conflict",
                        format!("flow run {flow_run_id} contains more than one recorded argsHash"),
                    )
                    .with_details(json!({
                        "flowRunId": flow_run_id,
                        "argsHashes": args_hashes,
                    })));
                }
                if catalog_hashes.len() > 1 {
                    return Err(ClientError::new(
                        "catalog-history-conflict",
                        format!(
                            "flow run {flow_run_id} contains more than one recorded catalogHash"
                        ),
                    )
                    .with_details(json!({
                        "flowRunId": flow_run_id,
                        "catalogHashes": catalog_hashes,
                    })));
                }
                return Ok(RunInspection {
                    script_hash: script_hashes.into_iter().next(),
                    args_hash: args_hashes.into_iter().next(),
                    catalog_hash: catalog_hashes.into_iter().next().flatten(),
                    supersede: None,
                });
            }
        })
    }

    fn submit<'a>(
        &'a self,
        submission: FlowSubmission,
    ) -> FlowFuture<'a, Result<Admission, ClientError>> {
        Box::pin(async move {
            let adapter = submission
                .spec
                .adapter
                .clone()
                .unwrap_or_else(|| "shell".to_owned());
            let result_expected = self
                .final_message_adapters
                .contains(&adapter)
                .then_some(ResultProjectionExpectation { adapter });
            let runner = self.runner.lock().await.clone();
            let payload = enqueue_payload(&submission, &runner)?;
            match self.call("queue.enqueue", payload).await {
                Ok(response) => {
                    validate_recorded_run_identity(&response, &submission)?;
                    let mut admission = parse_admission(&response)?;
                    if let Some(expectation) = result_expected {
                        self.result_expected.lock().await.insert(
                            (admission.task_uuid.clone(), admission.attempt),
                            expectation,
                        );
                    }
                    if let Some(result) = &mut admission.terminal {
                        self.enrich_terminal_result(result, admission.attempt)
                            .await?;
                    }
                    Ok(admission)
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
            let mut result = parse_node_result(&response, Disposition::Created)?;
            self.enrich_terminal_result(&mut result, attempt).await?;
            Ok(result)
        })
    }
}

fn reconnectable(method: &str, error: &WireIoError) -> bool {
    is_rearmable_rpc_error(method, error)
}

fn call_timeout_error(method: &str, retries: u32) -> WireIoError {
    WireIoError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("flow RPC {method} exceeded its total deadline after {retries} retries"),
    ))
}

fn retry_limit_error(method: &str, limit: u32, last_error: &WireIoError) -> WireIoError {
    WireIoError::Io(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("flow RPC {method} exhausted its {limit}-retry ceiling; last error: {last_error}"),
    ))
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
    if let Some(drv) = &spec.drv {
        payload.insert(
            "drv".to_owned(),
            serde_json::to_value(drv).map_err(json_client_error)?,
        );
    }
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
    insert_optional_string(&mut payload, "taskUuid", submission.task_uuid.as_deref());
    if let Some(runtime_max_sec) = spec.runtime_max_sec {
        payload.insert("runtimeMaxSec".to_owned(), json!(runtime_max_sec));
    }
    if let Some(task_uuid) = runner.task_uuid.as_deref() {
        payload.insert("parent".to_owned(), Value::String(task_uuid.to_owned()));
        if let Some(job_id) = runner.job_id.as_deref() {
            payload.insert("callerJobId".to_owned(), Value::String(job_id.to_owned()));
        }
    }
    if let Some(job_token) = runner.job_token.as_deref() {
        payload.insert(
            "callerJobToken".to_owned(),
            Value::String(job_token.to_owned()),
        );
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
    let terminal = if matches!(
        disposition,
        Disposition::Reused | Disposition::Substituted | Disposition::Terminal
    ) {
        Some(parse_node_result(value, disposition)?)
    } else {
        None
    };
    Ok(Admission {
        schema_version,
        disposition,
        task_uuid,
        task_ref: parse_task_ref(value)?,
        payload_hash,
        attempt,
        terminal,
        recorded_label: optional_str(value, &["recordedLabel"]).map(ToOwned::to_owned),
        reused_rejected: optional_str(value, &["reusedRejected"]).map(ToOwned::to_owned),
    })
}

fn validate_recorded_run_identity(
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
    validate_identity_capsule(recorded, submission)
}

fn validate_identity_capsule(
    recorded: &Map<String, Value>,
    submission: &FlowSubmission,
) -> Result<(), ClientError> {
    let recorded_script_hash = recorded
        .get("scriptHash")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            protocol_error("same-run replay admission omitted recordedOrchestration.scriptHash")
        })?;
    if recorded_script_hash != submission.orchestration.script_hash {
        return Err(changed_hash_error(
            "script-changed-mid-run",
            &submission.orchestration.flow_run_id,
            recorded_script_hash,
            &submission.orchestration.script_hash,
        ));
    }
    let recorded_args_hash = recorded
        .get("argsHash")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            protocol_error("same-run replay admission omitted recordedOrchestration.argsHash")
        })?;
    if recorded_args_hash != submission.orchestration.args_hash {
        return Err(changed_hash_error(
            "args-changed-mid-run",
            &submission.orchestration.flow_run_id,
            recorded_args_hash,
            &submission.orchestration.args_hash,
        ));
    }
    let recorded_catalog_hash = match recorded.get("catalogHash") {
        Some(Value::String(hash)) => Some(hash.as_str()),
        Some(Value::Null) => None,
        _ => {
            return Err(protocol_error(
                "same-run replay admission omitted recordedOrchestration.catalogHash or supplied an invalid value",
            ));
        }
    };
    if recorded_catalog_hash != submission.orchestration.catalog_hash.as_deref() {
        return Err(changed_catalog_identity_error(
            &submission.orchestration.flow_run_id,
            recorded_catalog_hash,
            submission.orchestration.catalog_hash.as_deref(),
        ));
    }
    Ok(())
}

fn changed_hash_error(
    code: &str,
    flow_run_id: &str,
    recorded_hash: &str,
    current_hash: &str,
) -> ClientError {
    ClientError::new(
        code,
        format!(
            "flow run {flow_run_id} is pinned to {recorded_hash}, not {current_hash}{}",
            tally_flow::identity_refusal_remedy_sentence(code, flow_run_id)
        ),
    )
    .with_details(json!({
        "flowRunId": flow_run_id,
        "recordedHash": recorded_hash,
        "currentHash": current_hash,
        "remedy": tally_flow::supersede_remedy(code, flow_run_id),
    }))
}

fn changed_catalog_identity_error(
    flow_run_id: &str,
    recorded_hash: Option<&str>,
    current_hash: Option<&str>,
) -> ClientError {
    let rendered_recorded = recorded_hash.unwrap_or("<none>");
    let rendered_current = current_hash.unwrap_or("<none>");
    ClientError::new(
        "catalog-changed-mid-run",
        format!(
            "flow run {flow_run_id} is pinned to {rendered_recorded}, not {rendered_current}{}",
            tally_flow::identity_refusal_remedy_sentence("catalog-changed-mid-run", flow_run_id)
        ),
    )
    .with_details(json!({
        "flowRunId": flow_run_id,
        "recordedHash": recorded_hash,
        "currentHash": current_hash,
        "remedy": tally_flow::supersede_remedy("catalog-changed-mid-run", flow_run_id),
    }))
}

fn parse_node_result(value: &Value, disposition: Disposition) -> Result<NodeResult, ClientError> {
    let task_uuid = required_str(value, &["taskUuid", "task_uuid"])?.to_owned();
    let verdict = parse_verdict(required_str(value, &["verdict"])?)?;
    let exit_code = optional_i32(value, &["exitCode", "exit_code"])?;
    let stderr_excerpt =
        optional_str(value, &["stderrExcerpt", "stderr_excerpt"]).map(str::to_owned);
    let stderr_truncated = optional_bool(value, &["stderrTruncated", "stderr_truncated"])?;
    let witness_seq = required_u64(value, &["witnessSeq", "witness_seq", "witness_lsn"])?;
    let model = optional_str(value, &["model"]).map(str::to_owned);
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
        task_ref: parse_task_ref(value)?,
        verdict,
        exit_code,
        stderr_excerpt,
        stderr_truncated,
        witness_seq,
        disposition,
        model,
        result,
        gates,
        error,
    })
}

fn parse_task_ref(value: &Value) -> Result<Option<TaskRef>, ClientError> {
    optional_str(value, &["taskRef"])
        .map(|task_ref| {
            TaskRef::new(task_ref.to_owned()).map_err(|error| {
                protocol_error(format!("response contains an invalid taskRef: {error}"))
            })
        })
        .transpose()
}

fn projected_result(value: &Value) -> Option<Value> {
    value
        .get("result")
        .cloned()
        .or_else(|| value.get("finalMessage").cloned())
        .map(|result| match result {
            Value::String(text) => decode_projected_result(&text),
            result => result,
        })
}

fn decode_projected_result(message: &str) -> Value {
    serde_json::from_str(message).unwrap_or_else(|_| Value::String(message.to_owned()))
}

fn parse_disposition(value: &str) -> Result<Disposition, ClientError> {
    match value {
        "created" => Ok(Disposition::Created),
        "attached" => Ok(Disposition::Attached),
        "reused" => Ok(Disposition::Reused),
        "substituted" => Ok(Disposition::Substituted),
        "terminal" => Ok(Disposition::Terminal),
        other => Err(protocol_error(format!(
            "unknown enqueue disposition {other:?}"
        ))),
    }
}

fn parse_verdict(value: &str) -> Result<Verdict, ClientError> {
    match value {
        "pass" => Ok(Verdict::Pass),
        "substituted" => Ok(Verdict::Substituted),
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
            let recorded_orchestration = recorded
                .get("orchestration")
                .and_then(Value::as_object)
                .expect("matching replay candidates carry an orchestration object");
            if let Err(error) = validate_identity_capsule(recorded_orchestration, submission) {
                return error;
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
                WireErrorCode::FlowLineageConflict => "flow-lineage-conflict",
                WireErrorCode::FlowLineageUnusable => "flow-lineage-unusable",
                WireErrorCode::StorageBudgetExceeded => "storage-budget-exceeded",
                WireErrorCode::StorageMonitorUnavailable => "storage-monitor-unavailable",
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

fn optional_bool(value: &Value, names: &[&str]) -> Result<Option<bool>, ClientError> {
    let Some(value) = names.iter().find_map(|name| value.get(*name)) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| protocol_error(format!("response field {} is not a boolean", names[0])))
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
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::rc::Rc;
    use tally_core::adapters::AdapterJobOptions;
    use tally_core::brief::PreparedBrief;
    use tally_core::config::Priority;
    use tally_core::provenance::Orchestration as KernelOrchestration;
    use tally_core::taskdb::{
        AdmissionOrigin, EnqueueSource, GhOrigin, RelatedTriggerOutcome, WorkspaceMetadata,
    };
    use tally_core::wire::{
        canonical_payload, canonical_payload_hash, EnqueuePayload, GuardrailConfig, GuardrailState,
        ProducerDefaults, ResolvedEnqueue, ENQUEUE_PAYLOAD_FIELDS,
    };
    use tally_core::witness::{
        Derivation as KernelDerivation, DerivationOutput as KernelDrvOutput,
    };
    use tally_flow::{
        flow_canonical_payload_fields, run_script, Derivation, FlowEnqueueFieldDisposition,
        NodeSpec, NodeWireProjection, Orchestration, RunOptions, SelectionProvenance,
        VecLifecycleSink, FLOW_ENQUEUE_FIELD_PARITY, NODE_SPEC_FIELD_CONTRACT,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    fn submission() -> FlowSubmission {
        FlowSubmission {
            mode: "full".to_owned(),
            dedup_key: "flow:00000000-0000-4000-8000-000000000047:0".to_owned(),
            payload_hash: "sha256:expected".to_owned(),
            task_uuid: None,
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
                drv: None,
                evidence_class: None,
                manifest_hash: None,
                workspace: None,
                brief: None,
                key: None,
                dedup_key: None,
                label: Some("first".to_owned()),
                task_ref: Some(TaskRef::new("crm/t07").unwrap()),
                env: Default::default(),
                approval_policy: None,
                sandbox_policy: None,
                model: None,
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
                args_hash: "sha256:args".to_owned(),
                catalog_hash: None,
                node_ordinal: 0,
                node_label: Some("first".to_owned()),
                task_ref: Some(TaskRef::new("crm/t07").unwrap()),
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

    #[derive(Default)]
    struct CapturingClient {
        submission: RefCell<Option<FlowSubmission>>,
    }

    impl FlowClient for CapturingClient {
        fn inspect_run<'a>(
            &'a self,
            _flow_run_id: &'a str,
        ) -> FlowFuture<'a, Result<RunInspection, ClientError>> {
            Box::pin(std::future::ready(Ok(RunInspection::default())))
        }

        fn submit<'a>(
            &'a self,
            submission: FlowSubmission,
        ) -> FlowFuture<'a, Result<Admission, ClientError>> {
            let payload_hash = submission.payload_hash.clone();
            let label = submission.spec.label.clone();
            let task_ref = submission.orchestration.task_ref.clone();
            self.submission.replace(Some(submission));
            Box::pin(std::future::ready(Ok(Admission {
                schema_version: 1,
                disposition: Disposition::Created,
                task_uuid: "00000000-0000-4000-8000-000000000073".to_owned(),
                task_ref: task_ref.clone(),
                payload_hash,
                attempt: 1,
                terminal: Some(NodeResult {
                    task_uuid: "00000000-0000-4000-8000-000000000073".to_owned(),
                    task_ref,
                    verdict: Verdict::Pass,
                    exit_code: Some(0),
                    stderr_excerpt: None,
                    stderr_truncated: None,
                    witness_seq: 1,
                    disposition: Disposition::Created,
                    model: None,
                    result: Some(json!({"ok": true})),
                    gates: None,
                    error: None,
                }),
                recorded_label: label,
                reused_rejected: None,
            })))
        }

        fn await_terminal<'a>(
            &'a self,
            _task_uuid: &'a str,
            _attempt: u32,
        ) -> FlowFuture<'a, Result<NodeResult, ClientError>> {
            Box::pin(std::future::ready(Err(ClientError::new(
                "unexpected-await",
                "capturing client admissions are terminal inline",
            ))))
        }
    }

    #[test]
    fn flow_and_kernel_use_the_same_task_ref_type() {
        let flow_task_ref = tally_flow::TaskRef::new("crm/t07").unwrap();
        let kernel_task_ref: tally_core::provenance::TaskRef = flow_task_ref.clone();
        assert_eq!(kernel_task_ref.campaign(), "crm");
        assert_eq!(kernel_task_ref.task_id(), "t07");
        assert_eq!(flow_task_ref, kernel_task_ref);
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
                job_token: Some("ab".repeat(32)),
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
        assert_eq!(payload["callerJobToken"], "ab".repeat(32));
        assert_eq!(payload["orchestration"]["nodeOrdinal"], 0);
        assert_eq!(payload["orchestration"]["argsHash"], "sha256:args");
        assert_eq!(payload["orchestration"]["taskRef"], "crm/t07");
        assert!(payload["orchestration"]["catalogHash"].is_null());
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
    fn every_wire_exposed_node_field_is_rendered_from_the_shared_contract() {
        let mut submission = submission();
        submission.spec.prompt = Some("mission".to_owned());
        submission.spec.executor = Some("worker-a".to_owned());
        submission.spec.runtime_max_sec = Some(30);
        submission.spec.drv = Some(
            serde_json::from_value(json!({
                "drvPath": "/nix/store/00000000000000000000000000000000-node.drv",
                "outputs": [{
                    "name": "out",
                    "path": "/nix/store/11111111111111111111111111111111-node"
                }]
            }))
            .unwrap(),
        );
        submission.spec.evidence_class = Some(json!({"kind": "contract"}));
        submission.spec.manifest_hash = Some("sha256:manifest".to_owned());
        submission.spec.workspace = Some(json!({"repo": "mecattaf/tally.nix"}));
        submission.spec.brief = Some(json!({"mission": "test"}));
        submission.spec.key = Some("node".to_owned());
        submission.spec.dedup_key = Some("explicit-dedup".to_owned());
        submission.spec.label = Some("node-label".to_owned());
        submission.spec.env = BTreeMap::from([("SAFE".to_owned(), "yes".to_owned())]);
        submission.spec.result_schema = Some(json!({"type": "object"}));
        submission.spec.selection = Some(SelectionProvenance {
            selector: "pooled".to_owned(),
            catalog_hash: "sha256:catalog".to_owned(),
            member_id: "worker-a".to_owned(),
            members: vec!["worker-a".to_owned()],
        });

        let payload = enqueue_payload(&submission, &RunnerIdentity::default()).unwrap();
        for field in NODE_SPEC_FIELD_CONTRACT {
            let wire_field = match field.wire {
                NodeWireProjection::Field(field) | NodeWireProjection::NormalizedInto(field) => {
                    field
                }
                NodeWireProjection::Excluded(_) => continue,
            };
            assert!(
                payload.get(wire_field).is_some(),
                "NodeSpec field {} is not rendered to {wire_field}",
                field.json_name
            );
        }
    }

    #[test]
    fn every_kernel_enqueue_field_has_a_recorded_flow_disposition() {
        let classified_fields = FLOW_ENQUEUE_FIELD_PARITY
            .iter()
            .map(|field| field.kernel_field)
            .collect::<Vec<_>>();
        assert_eq!(ENQUEUE_PAYLOAD_FIELDS, classified_fields);

        for field in FLOW_ENQUEUE_FIELD_PARITY {
            let explanation = match field.disposition {
                FlowEnqueueFieldDisposition::Exposed(via)
                | FlowEnqueueFieldDisposition::Excluded(via) => via,
            };
            assert!(
                !explanation.trim().is_empty(),
                "kernel enqueue field {} has an empty flow disposition",
                field.kernel_field
            );
        }

        let consumption = FLOW_ENQUEUE_FIELD_PARITY
            .iter()
            .find(|field| field.kernel_field == "consumptionEstimate")
            .unwrap();
        let FlowEnqueueFieldDisposition::Excluded(reason) = consumption.disposition else {
            panic!("consumptionEstimate must remain excluded from flow nodes");
        };
        assert!(reason.contains("windowed-consumption"));
        assert!(reason.contains("priorities"));
    }

    #[test]
    fn fully_populated_engine_and_wire_payload_hashes_match() {
        let source = r#"
export const meta = {
  name: "hash-contract",
  description: "engine and daemon canonical payload parity",
  pools: ["alpha"],
  argsSchema: {type: "object", additionalProperties: false},
  selectors: ["hash-member"],
  maxNodes: 1
};

(async () => {
  const [member] = members("hash-member", {count: 1});
  return local("compare both canonical payload implementations", {
    member,
    executor: "remote-a",
    priority: "high",
    runtimeMaxSec: 37,
    evidence: ["exit:0"],
    evidenceClass: {kind: "contract", revision: 2},
    manifestHash: "sha256:manifest-contract",
    workspace: {
      repo: "mecattaf/tally.nix",
      baseRev: "0123456789abcdef",
      branch: "t-145-u2",
      worktreePath: "/work/tally-t145"
    },
    key: "fully-populated",
    label: "fully-populated-node",
    env: {FROM_SPEC: "yes"},
    resultSchema: {type: "object", required: ["ok"]}
  });
})()
"#;
        let client = Rc::new(CapturingClient::default());
        let mut options = RunOptions::new("00000000-0000-4000-8000-000000000074", json!({}));
        options.pool_credentials.insert(
            "alpha".to_owned(),
            BTreeMap::from([
                (
                    "api-token".to_owned(),
                    PathBuf::from("/run/credentials/alpha-api-token"),
                ),
                (
                    "signing-key".to_owned(),
                    PathBuf::from("/run/credentials/alpha-signing-key"),
                ),
            ]),
        );
        options.catalog = Some(
            serde_json::from_value(json!({
                "version": 1,
                "members": [{
                    "id": "hash-member-a",
                    "family": "contract-family",
                    "maker": "contract-maker",
                    "classes": ["hash-member"],
                    "adapter": "codex",
                    "pools": ["alpha"],
                    "launch": {
                        "prePromptArgv": ["--contract"],
                        "environment": {"FROM_OPTIONS": "yes"},
                        "approvalPolicy": "never",
                        "sandboxPolicy": "workspace-write",
                        "model": "provider/model-v2",
                        "effort": "high"
                    }
                }]
            }))
            .unwrap(),
        );
        options.catalog_hash = Some("sha256:catalog-contract".to_owned());
        run_script(
            source,
            Some(Path::new("hash-contract.js")),
            client.clone(),
            Rc::new(VecLifecycleSink::default()),
            options,
        )
        .unwrap();
        let submission = client.submission.borrow().clone().unwrap();
        assert_eq!(submission.credentials.len(), 2);
        assert!(submission.spec.brief.is_some());
        assert!(submission.spec.workspace.is_some());
        assert_eq!(
            submission.spec.adapter_options.as_ref().unwrap()["model"],
            "provider/model-v2"
        );

        let mut payload: EnqueuePayload = serde_json::from_value(
            enqueue_payload(&submission, &RunnerIdentity::default()).unwrap(),
        )
        .unwrap();
        let brief = PreparedBrief::from_value(payload.brief.take().unwrap()).unwrap();
        let defaults = ProducerDefaults {
            pools: vec!["alpha".to_owned()],
            executor: Some("remote-a".to_owned()),
            priority: Priority::High,
            adapter: "codex".to_owned(),
            source: EnqueueSource::Orchestrator,
            cwd: None,
            workspace: None,
            adapter_options: AdapterJobOptions::default(),
        };
        let mut resolved = GuardrailState::new(GuardrailConfig::default())
            .unwrap()
            .validate_enqueue(payload, &defaults)
            .unwrap();
        resolved.brief_hash = Some(brief.hash().to_owned());

        assert_eq!(
            canonical_payload_hash(&resolved).unwrap(),
            submission.payload_hash
        );
    }

    #[test]
    fn kernel_canonical_fields_match_the_node_contract_in_preserved_order() {
        let resolved = ResolvedEnqueue {
            argv: vec!["tool".to_owned(), "--flag".to_owned()],
            pools: vec!["alpha".to_owned(), "zeta".to_owned()],
            executor: Some("worker-a".to_owned()),
            priority: Priority::High,
            adapter: "codex".to_owned(),
            // These kernel-only hashed fields are rejected for full-mode flow
            // submissions until NodeSpec explicitly exposes them.
            cwd: None,
            workspace: Some(WorkspaceMetadata {
                repo: "mecattaf/tally.nix".to_owned(),
                base_rev: "origin/main".to_owned(),
                branch: "t-145-u3".to_owned(),
                worktree_path: PathBuf::from("/work/tally-t145"),
            }),
            adapter_options: AdapterJobOptions {
                pre_prompt_argv: vec!["--json".to_owned()],
                environment: BTreeMap::from([("NO_COLOR".to_owned(), "1".to_owned())]),
                approval_policy: Some("never".to_owned()),
                sandbox_policy: Some("workspace-write".to_owned()),
                model: Some("provider/model".to_owned()),
                effort: Some("high".to_owned()),
            },
            gate_manifest: None,
            brief_hash: Some(format!("sha256:{}", "b".repeat(64))),
            resume_from: Some("00000000-0000-4000-8000-000000000141".to_owned()),
            source: EnqueueSource::Orchestrator,
            dedup_key: Some("flow:contract:0".to_owned()),
            orchestration: Some(
                serde_json::from_value::<KernelOrchestration>(json!({
                    "flowRunId": "00000000-0000-4000-8000-000000000142",
                    "maxNodes": 1
                }))
                .unwrap(),
            ),
            parent: Some("00000000-0000-4000-8000-000000000143".to_owned()),
            evidence: vec!["exit:0".to_owned()],
            drv: Some(KernelDerivation {
                drv_path: "/nix/store/00000000000000000000000000000000-node.drv".to_owned(),
                outputs: vec![KernelDrvOutput {
                    name: "out".to_owned(),
                    path: "/nix/store/11111111111111111111111111111111-node".to_owned(),
                }],
            }),
            evidence_class: Some(json!({"kind": "contract"})),
            manifest_hash: Some("sha256:manifest".to_owned()),
            consumption_estimate: Some(17),
            runtime_max_sec: Some(300),
            no_enqueue: true,
            credentials: BTreeMap::from([(
                "token".to_owned(),
                PathBuf::from("/run/credentials/token"),
            )]),
            origin: AdmissionOrigin::direct(EnqueueSource::Orchestrator),
            gh_origin: Some(
                serde_json::from_value::<GhOrigin>(json!({
                    "producer": "github",
                    "source": "notifications",
                    "actor": "maintainer",
                    "selfActor": "tally-bot",
                    "actorExclude": "tally-bot"
                }))
                .unwrap(),
            ),
            task_uuid: Some("00000000-0000-4000-8000-000000000144".to_owned()),
            related_trigger: Some(related_trigger()),
            depth: 4,
            wait: true,
        };
        let canonical: Value =
            serde_json::from_slice(&canonical_payload(&resolved).unwrap()).unwrap();
        let kernel_fields = canonical
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        assert_eq!(kernel_fields, flow_canonical_payload_fields());
    }

    #[test]
    fn drv_payload_matches_the_landed_kernel_contract_and_hash() {
        const DRV: &str = "/nix/store/00000000000000000000000000000000-fixture.drv";
        const DEV: &str = "/nix/store/11111111111111111111111111111111-fixture-dev";
        const OUT: &str = "/nix/store/22222222222222222222222222222222-fixture";
        let mut submission = submission();
        submission.dedup_key = format!("drv:{DRV}");
        submission.payload_hash =
            "sha256:7420a9161793b05545bbb806bf1449a9554f756b8e4d800718050b6447b31f7f".to_owned();
        submission.task_uuid = Some("35c1f3a2-0ec5-53bf-8019-62ac60ca5bb0".to_owned());
        submission.spec.argv = Some(vec![
            "nix".to_owned(),
            "build".to_owned(),
            "--no-link".to_owned(),
            format!("{DRV}^*"),
        ]);
        submission.spec.pools = vec!["build".to_owned()];
        submission.spec.priority = None;
        submission.spec.evidence = vec![format!("store:{DEV}"), format!("store:{OUT}")];
        submission.spec.drv = Some(
            serde_json::from_value::<Derivation>(json!({
                "drvPath": DRV,
                "outputs": [
                    {"name": "dev", "path": DEV},
                    {"name": "out", "path": OUT}
                ]
            }))
            .unwrap(),
        );
        submission.spec.label = None;
        submission.spec.adapter_options = Some(json!({"prePromptArgv": [], "environment": {}}));

        let raw = enqueue_payload(&submission, &RunnerIdentity::default()).unwrap();
        assert_eq!(raw["taskUuid"], submission.task_uuid.unwrap());
        assert_eq!(raw["drv"]["drvPath"], DRV);
        assert_eq!(
            raw["evidence"],
            json!([format!("store:{DEV}"), format!("store:{OUT}")])
        );

        let payload: EnqueuePayload = serde_json::from_value(raw).unwrap();
        let defaults = ProducerDefaults {
            pools: vec!["build".to_owned()],
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
            canonical_payload_hash(&resolved).unwrap(),
            submission.payload_hash
        );
    }

    #[test]
    fn substituted_admission_is_an_inline_success() {
        let admission = parse_admission(&json!({
            "schemaVersion": 1,
            "disposition": "substituted",
            "taskUuid": "35c1f3a2-0ec5-53bf-8019-62ac60ca5bb0",
            "taskRef": "crm/t07",
            "payloadHash": "sha256:payload",
            "attempt": 1,
            "verdict": "substituted",
            "exitCode": 0,
            "witnessSeq": 7
        }))
        .unwrap();
        assert_eq!(admission.disposition, Disposition::Substituted);
        assert_eq!(admission.task_ref.unwrap().as_str(), "crm/t07");
        let terminal = admission.terminal.unwrap();
        assert_eq!(terminal.task_ref.unwrap().as_str(), "crm/t07");
        assert_eq!(terminal.verdict, Verdict::Substituted);
        assert!(terminal.verdict.is_pass());
        assert_eq!(terminal.disposition, Disposition::Substituted);
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
                        "argsHash": "sha256:args",
                        "catalogHash": null,
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
    fn conflict_identity_checks_args_and_catalog_before_payload_divergence() {
        let conflict = |args_hash: &str, catalog_hash: Value| {
            WireIoError::Rpc(
                WireErrorCode::DedupKeyConflict,
                "dedup-key-conflict".to_owned(),
                Some(json!({
                    "existing": [{
                        "taskUuid": "00000000-0000-4000-8000-000000000049",
                        "payloadHash": "sha256:other-payload",
                        "orchestration": {
                            "flowRunId": "00000000-0000-4000-8000-000000000047",
                            "scriptHash": "sha256:script",
                            "argsHash": args_hash,
                            "catalogHash": catalog_hash,
                            "nodeOrdinal": 0
                        }
                    }]
                })),
            )
        };

        let args = submission_error(&submission(), conflict("sha256:other-args", Value::Null));
        assert_eq!(args.code, "args-changed-mid-run");

        let catalog = submission_error(
            &submission(),
            conflict("sha256:args", json!("sha256:other-catalog")),
        );
        assert_eq!(catalog.code, "catalog-changed-mid-run");
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
        let error = validate_recorded_run_identity(&response, &submission()).unwrap_err();
        assert_eq!(error.code, "script-changed-mid-run");
        assert_eq!(
            error.details.as_ref().unwrap()["recordedHash"],
            "sha256:other-script"
        );

        let mut cross_run = response;
        cross_run["recordedOrchestration"]["flowRunId"] =
            json!("00000000-0000-4000-8000-000000000099");
        validate_recorded_run_identity(&cross_run, &submission()).unwrap();
    }

    #[test]
    fn same_run_admission_pins_args_before_catalog() {
        let mut response = json!({
            "schemaVersion": 1,
            "disposition": "attached",
            "task_uuid": "00000000-0000-4000-8000-000000000049",
            "payloadHash": "sha256:expected",
            "attempt": 1,
            "recordedOrchestration": {
                "flowName": "fixture",
                "flowRunId": "00000000-0000-4000-8000-000000000047",
                "scriptHash": "sha256:script",
                "argsHash": "sha256:other-args",
                "catalogHash": "sha256:other-catalog",
                "nodeOrdinal": 0,
                "maxNodes": 10
            }
        });
        let error = validate_recorded_run_identity(&response, &submission()).unwrap_err();
        assert_eq!(error.code, "args-changed-mid-run");
        assert_eq!(
            error.details.as_ref().unwrap()["recordedHash"],
            "sha256:other-args"
        );

        response["recordedOrchestration"]["argsHash"] = json!("sha256:args");
        let error = validate_recorded_run_identity(&response, &submission()).unwrap_err();
        assert_eq!(error.code, "catalog-changed-mid-run");
        assert_eq!(
            error.details.as_ref().unwrap()["recordedHash"],
            "sha256:other-catalog"
        );
        assert!(error.details.as_ref().unwrap()["currentHash"].is_null());

        response["recordedOrchestration"]["catalogHash"] = Value::Null;
        validate_recorded_run_identity(&response, &submission()).unwrap();
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
                "stderr_excerpt": "captured detail\n",
                "stderr_truncated": false,
                "witness_seq": 8
            }),
            Disposition::Created,
        )
        .unwrap();
        assert_eq!(awaited.witness_seq, 8);
        assert_eq!(awaited.stderr_excerpt.as_deref(), Some("captured detail\n"));
        assert_eq!(awaited.stderr_truncated, Some(false));
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
    fn live_reconnect_backoff_starts_at_fifty_milliseconds_and_caps_at_two_seconds() {
        assert_eq!(
            live_retry_delay(2, LIVE_RETRY_BASE_DELAY),
            Duration::from_millis(50)
        );
        assert_eq!(
            live_retry_delay(3, LIVE_RETRY_BASE_DELAY),
            Duration::from_millis(100)
        );
        assert_eq!(
            live_retry_delay(8, LIVE_RETRY_BASE_DELAY),
            LIVE_RETRY_MAX_DELAY
        );
        assert_eq!(
            live_retry_delay(64, LIVE_RETRY_BASE_DELAY),
            LIVE_RETRY_MAX_DELAY
        );
    }

    #[tokio::test]
    async fn persistent_retryable_errors_obey_backoff_and_retry_ceiling() {
        const RETRY_LIMIT: u32 = 2;
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let mut requests = 0;
            for _ in 0..=RETRY_LIMIT {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let request: Value =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                requests += 1;
                let mut response = serde_json::to_vec(&json!({
                    "id": request["id"],
                    "error": {"code": "timeout", "message": "retry later"}
                }))
                .unwrap();
                response.push(b'\n');
                write.write_all(&response).await.unwrap();
            }
            requests
        });

        let lifecycle = Rc::new(VecLifecycleSink::default());
        let mut client = LiveFlowClient::new(&socket, 16 * 1024 * 1024, RunnerIdentity::default())
            .with_lifecycle_sink(lifecycle.clone());
        client.call_timeout = Duration::from_secs(1);
        client.retry_limit = RETRY_LIMIT;
        client.retry_base_delay = Duration::from_millis(15);
        let started = Instant::now();
        let error = client.call("query.status", json!({})).await.unwrap_err();
        let elapsed = started.elapsed();
        match error {
            WireIoError::Io(error) => {
                assert_eq!(error.kind(), io::ErrorKind::TimedOut);
                assert!(error.to_string().contains("2-retry ceiling"));
            }
            other => panic!("expected bounded retry timeout, got {other:?}"),
        }
        assert!(elapsed >= Duration::from_millis(10));
        assert!(elapsed < Duration::from_millis(500));
        assert_eq!(server.await.unwrap(), RETRY_LIMIT + 1);
        assert_eq!(
            lifecycle.events(),
            [json!({
                "type": "flow-rpc-reconnect",
                "method": "query.status",
                "attempt": 1,
                "error": "RPC error Timeout: retry later",
            })]
        );
    }

    #[tokio::test]
    async fn total_call_deadline_bounds_a_server_that_never_replies() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            lines.next_line().await.unwrap().unwrap();
            std::future::pending::<()>().await;
        });

        let mut client = LiveFlowClient::new(&socket, 16 * 1024 * 1024, RunnerIdentity::default());
        client.call_timeout = Duration::from_millis(40);
        let started = Instant::now();
        let error = client.call("query.status", json!({})).await.unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(500));
        match error {
            WireIoError::Io(error) => assert_eq!(error.kind(), io::ErrorKind::TimedOut),
            other => panic!("expected total call deadline, got {other:?}"),
        }
        server.abort();
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
    async fn required_projection_timeout_bounds_a_blocked_query_and_names_the_capture() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("tally.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, _write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            lines.next_line().await.unwrap().unwrap();
            std::future::pending::<()>().await;
        });

        let mut client = LiveFlowClient::new(&socket, 16 * 1024 * 1024, RunnerIdentity::default());
        client.result_projection_timeout = Duration::from_millis(40);
        let task_uuid = "00000000-0000-4000-8000-000000000053";
        client.result_expected.lock().await.insert(
            (task_uuid.to_owned(), 1),
            ResultProjectionExpectation {
                adapter: "structured".to_owned(),
            },
        );
        let mut result = NodeResult {
            task_uuid: task_uuid.to_owned(),
            task_ref: None,
            verdict: Verdict::Pass,
            exit_code: Some(0),
            stderr_excerpt: None,
            stderr_truncated: None,
            witness_seq: 1,
            disposition: Disposition::Created,
            model: None,
            result: None,
            gates: None,
            error: None,
        };

        let started = Instant::now();
        client.enrich_terminal_result(&mut result, 1).await.unwrap();
        assert!(started.elapsed() < Duration::from_millis(500));
        let error = result.error.unwrap();
        assert_eq!(error.code, "result-projection-timeout");
        assert!(error.message.contains("finalMessage"));
        assert_eq!(error.details.unwrap()["adapter"], "structured");
        server.abort();
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
