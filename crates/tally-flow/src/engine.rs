use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::task::{Poll, Waker};

use boa_engine::builtins::promise::{OperationType, PromiseState};
use boa_engine::context::{ContextBuilder, HostHooks};
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::realm::Realm;
use boa_engine::{
    js_string, Context, Finalize, JsData, JsError, JsNativeError, JsObject, JsResult, JsString,
    JsValue, NativeFunction, Script, Source, Trace,
};
use serde_json::{json, Map, Value};

use crate::catalog::sha256;
use crate::dialect::validate_instance;
use crate::executor::FlowJobExecutor;
use crate::model::SubmissionPlan;
use crate::{
    check_script, resolve_members, Catalog, CheckOptions, Disposition, FlowClient, FlowError,
    FlowSubmission, Meta, NodeFailure, NodeResult, NodeSpec, Orchestration, RunReport,
    SelectionProvenance, SelectorOptions, SourceLocation, BRIEF_SENTINEL, DEFAULT_MAX_NODES,
    ENGINE_LOOP_LIMIT, ENGINE_RECURSION_LIMIT,
};

const BOOTSTRAP: &str = include_str!("bootstrap.js");
const BOOTSTRAP_PATH: &str = "<tally-flow-bootstrap>";
const RUNTIME_ERROR_LOCATION: SourceLocation = SourceLocation::new(1, 1);

/// Receives the runner's structured lifecycle stream.
pub trait LifecycleSink {
    fn emit(&self, event: Value) -> Result<(), FlowError>;
}

/// In-memory lifecycle capture used by tests and embedders.
#[derive(Debug, Default)]
pub struct VecLifecycleSink {
    events: RefCell<Vec<Value>>,
}

impl VecLifecycleSink {
    #[must_use]
    pub fn events(&self) -> Vec<Value> {
        self.events.borrow().clone()
    }
}

impl LifecycleSink for VecLifecycleSink {
    fn emit(&self, event: Value) -> Result<(), FlowError> {
        self.events.borrow_mut().push(event);
        Ok(())
    }
}

/// Stable inputs to one stateless execution of a flow script.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub args: Value,
    pub flow_run_id: String,
    pub max_nodes: u32,
    pub catalog: Option<Catalog>,
    pub catalog_hash: Option<String>,
    pub pool_credentials: BTreeMap<String, BTreeMap<String, PathBuf>>,
    pub adapter_skill_revisions: BTreeMap<String, String>,
}

impl RunOptions {
    #[must_use]
    pub fn new(flow_run_id: impl Into<String>, args: Value) -> Self {
        Self {
            args,
            flow_run_id: flow_run_id.into(),
            max_nodes: DEFAULT_MAX_NODES,
            catalog: None,
            catalog_hash: None,
            pool_credentials: BTreeMap::new(),
            adapter_skill_revisions: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Default)]
struct NodeRevisions {
    prompt_revision: Option<String>,
    skill_revision: Option<String>,
}

#[derive(Clone, Finalize, JsData, Trace)]
struct HostHandle {
    #[unsafe_ignore_trace]
    shared: Rc<HostShared>,
}

#[derive(Default)]
struct HostState {
    next_ordinal: u64,
    admission_frontier: u64,
    admission_wakers: BTreeMap<u64, Waker>,
    explicit_keys: BTreeSet<String>,
    resolved_selections: BTreeSet<(String, String, Vec<String>)>,
    ordinal_keys: Vec<String>,
    iteration_counts: BTreeMap<SourceLocation, u32>,
    pending_logs: BTreeMap<u64, Vec<Value>>,
    fatal: Option<FlowError>,
    ready_observations: BTreeMap<(u64, u64), u64>,
    observation_wakers: HashMap<u64, Waker>,
    granted_observations: HashSet<u64>,
    next_observation_token: u64,
    observation_order: Vec<u64>,
}

pub(crate) struct HostShared {
    client: Rc<dyn FlowClient>,
    sink: Rc<dyn LifecycleSink>,
    meta: Meta,
    flow_run_id: String,
    script_hash: String,
    effective_max_nodes: u32,
    host_call_sites: Vec<SourceLocation>,
    catalog: Option<Catalog>,
    catalog_hash: Option<String>,
    pool_credentials: BTreeMap<String, BTreeMap<String, PathBuf>>,
    adapter_skill_revisions: BTreeMap<String, String>,
    state: RefCell<HostState>,
}

impl HostShared {
    pub(crate) fn fatal_error(&self) -> Option<FlowError> {
        self.state.borrow().fatal.clone()
    }

    fn set_fatal(&self, error: FlowError) {
        let mut state = self.state.borrow_mut();
        if state.fatal.is_none() {
            state.fatal = Some(error);
        }
        for (_, waker) in std::mem::take(&mut state.admission_wakers) {
            waker.wake();
        }
        for (_, waker) in std::mem::take(&mut state.observation_wakers) {
            waker.wake();
        }
    }

    pub(crate) fn has_ready_observation(&self) -> bool {
        let state = self.state.borrow();
        state.admission_frontier == state.next_ordinal && !state.ready_observations.is_empty()
    }

    pub(crate) fn release_ready_observation(&self) {
        let mut state = self.state.borrow_mut();
        if state.admission_frontier != state.next_ordinal {
            return;
        }
        let Some((&(witness_seq, _), &token)) = state.ready_observations.first_key_value() else {
            return;
        };
        state.ready_observations.pop_first();
        state.granted_observations.insert(token);
        state.observation_order.push(witness_seq);
        if let Some(waker) = state.observation_wakers.remove(&token) {
            waker.wake();
        }
    }

    async fn wait_for_admission(&self, ordinal: u64) -> Result<(), FlowError> {
        std::future::poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if let Some(error) = &state.fatal {
                return Poll::Ready(Err(error.clone()));
            }
            if state.admission_frontier == ordinal {
                return Poll::Ready(Ok(()));
            }
            state.admission_wakers.insert(ordinal, cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    fn finish_admission(
        &self,
        ordinal: u64,
        disposition: Option<Disposition>,
    ) -> Result<(), FlowError> {
        let logs = {
            let mut state = self.state.borrow_mut();
            if state.admission_frontier != ordinal {
                return Err(FlowError::new(
                    "FlowReplayError",
                    "admission-order-corrupt",
                    format!(
                        "admission ordinal {ordinal} completed while frontier was {}",
                        state.admission_frontier
                    ),
                )
                .with_ordinal(ordinal));
            }
            let logs = state.pending_logs.remove(&ordinal).unwrap_or_default();
            state.admission_frontier += 1;
            let next = state.admission_frontier;
            if let Some(waker) = state.admission_wakers.remove(&next) {
                waker.wake();
            }
            logs
        };

        if disposition == Some(Disposition::Created) {
            for event in logs {
                self.sink.emit(event)?;
            }
        }
        Ok(())
    }

    async fn observe(&self, witness_seq: u64, ordinal: u64) -> Result<(), FlowError> {
        let token = {
            let mut state = self.state.borrow_mut();
            let token = state.next_observation_token;
            state.next_observation_token += 1;
            state
                .ready_observations
                .insert((witness_seq, ordinal), token);
            token
        };

        std::future::poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if let Some(error) = &state.fatal {
                return Poll::Ready(Err(error.clone()));
            }
            if state.granted_observations.remove(&token) {
                return Poll::Ready(Ok(()));
            }
            state.observation_wakers.insert(token, cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    fn queue_log(&self, message: Value, location: SourceLocation) {
        let mut state = self.state.borrow_mut();
        let frontier = state.next_ordinal;
        state.pending_logs.entry(frontier).or_default().push(json!({
            "type": "log",
            "flowRunId": self.flow_run_id,
            "frontier": frontier,
            "message": message,
            "line": location.line,
            "column": location.column,
        }));
    }

    fn record_selection(&self, selector: &str, catalog_hash: &str, members: &[String]) {
        self.state.borrow_mut().resolved_selections.insert((
            selector.to_owned(),
            catalog_hash.to_owned(),
            members.to_vec(),
        ));
    }

    fn selection_was_resolved(&self, selection: &SelectionProvenance) -> bool {
        self.state.borrow().resolved_selections.contains(&(
            selection.selector.clone(),
            selection.catalog_hash.clone(),
            selection.members.clone(),
        )) && selection
            .members
            .iter()
            .any(|member| member == &selection.member_id)
    }

    fn flush_final_logs(&self) -> Result<(), FlowError> {
        let events = {
            let mut state = self.state.borrow_mut();
            let frontier = state.next_ordinal;
            state.pending_logs.remove(&frontier).unwrap_or_default()
        };
        for event in events {
            self.sink.emit(event)?;
        }
        Ok(())
    }

    fn frontier(&self) -> u64 {
        self.state.borrow().next_ordinal
    }

    fn annotate_frontier(&self, mut error: FlowError) -> FlowError {
        if error.ordinal.is_none() {
            error.ordinal = Some(self.frontier());
        }
        error
    }

    fn exact_call_site(&self, approximate: SourceLocation) -> SourceLocation {
        self.host_call_sites
            .iter()
            .copied()
            .filter(|location| {
                location.line == approximate.line && location.column <= approximate.column
            })
            .max_by_key(|location| location.column)
            .unwrap_or(approximate)
    }

    fn report(&self, final_value: Option<Value>) -> RunReport {
        let state = self.state.borrow();
        RunReport {
            flow_run_id: self.flow_run_id.clone(),
            flow_name: self.meta.name.clone(),
            script_hash: self.script_hash.clone(),
            catalog_hash: self.catalog_hash.clone(),
            ordinal_keys: state.ordinal_keys.clone(),
            observation_order: state.observation_order.clone(),
            final_value,
        }
    }

    fn prepare_submission(
        &self,
        mut spec: NodeSpec,
        revisions: NodeRevisions,
        settle: bool,
        location: SourceLocation,
    ) -> Result<SubmissionPlan, FlowError> {
        validate_node_spec_shape(&spec, location)?;
        normalize_prompt(&mut spec, location)?;
        if spec.adapter.is_none() {
            spec.adapter = Some("shell".to_owned());
        }
        normalize_workspace(&mut spec, location)?;
        normalize_pools(&mut spec, &self.meta, location)?;
        normalize_adapter_options(&mut spec, location)?;

        if let Some(schema) = &spec.result_schema {
            jsonschema::validator_for(schema).map_err(|error| {
                FlowError::new(
                    "FlowSchemaError",
                    "result-schema-invalid",
                    format!("resultSchema is not a valid JSON Schema: {error}"),
                )
                .at(location)
            })?;
        }

        let mut state = self.state.borrow_mut();
        let count = state.iteration_counts.entry(location).or_default();
        *count += 1;
        if *count > self.meta.iteration_cap() {
            return Err(FlowError::new(
                "FlowLoopError",
                "iteration-cap",
                format!(
                    "job call at line {}, column {} was invoked {} times (cap {})",
                    location.line,
                    location.column,
                    *count,
                    self.meta.iteration_cap()
                ),
            )
            .at(location)
            .detail("count", *count)
            .detail("cap", self.meta.iteration_cap()));
        }

        let ordinal = state.next_ordinal;
        if spec.key.is_some() && spec.dedup_key.is_some() {
            return Err(FlowError::new(
                "FlowKeyError",
                "key-conflict",
                "spec.key and spec.dedupKey are mutually exclusive",
            )
            .at(location)
            .with_ordinal(ordinal));
        }
        let dedup_key = if let Some(key) = &spec.key {
            if !state.explicit_keys.insert(key.clone()) {
                return Err(FlowError::new(
                    "FlowKeyError",
                    "duplicate-key",
                    format!("flow-local key {key:?} is used more than once"),
                )
                .at(location)
                .with_ordinal(ordinal)
                .detail("key", key.clone()));
            }
            format!("flow:{}:k:{key}", self.flow_run_id)
        } else if let Some(key) = &spec.dedup_key {
            key.clone()
        } else {
            format!("flow:{}:{ordinal}", self.flow_run_id)
        };

        let result_schema = spec.result_schema.take();
        let credentials = resolve_pool_credentials(&spec.pools, &self.pool_credentials);
        let payload_hash = canonical_payload_hash(&spec, &credentials)?;
        let orchestration = Orchestration {
            flow_name: self.meta.name.clone(),
            flow_run_id: self.flow_run_id.clone(),
            script_hash: self.script_hash.clone(),
            node_ordinal: ordinal,
            node_label: spec.label.clone(),
            max_nodes: self.effective_max_nodes,
            prompt_revision: revisions.prompt_revision,
            skill_revision: revisions.skill_revision,
            selection: spec.selection.clone(),
        };
        let submission = FlowSubmission {
            mode: "full".to_owned(),
            dedup_key: dedup_key.clone(),
            payload_hash,
            credentials,
            spec,
            orchestration,
        };
        state.next_ordinal += 1;
        state.ordinal_keys.push(dedup_key);
        Ok(SubmissionPlan {
            submission,
            settle,
            result_schema,
            location,
            ordinal,
        })
    }

    fn agent_revisions(&self, adapter: &str, prompt: &str) -> NodeRevisions {
        NodeRevisions {
            prompt_revision: Some(sha256(prompt.as_bytes())),
            skill_revision: self.adapter_skill_revisions.get(adapter).cloned(),
        }
    }

    async fn execute_submission(&self, plan: SubmissionPlan) -> Result<NodeResult, FlowError> {
        self.wait_for_admission(plan.ordinal).await?;
        let expected_hash = plan.submission.payload_hash.clone();
        let expected_label = plan.submission.spec.label.clone();
        let admission = match self.client.submit(plan.submission).await {
            Ok(admission) => admission,
            Err(error) => {
                let fatal_replay = matches!(
                    error.code.as_str(),
                    "replay-divergence" | "script-changed-mid-run"
                );
                let flow_error = error.into_flow(plan.location, plan.ordinal);
                if fatal_replay {
                    self.set_fatal(flow_error.clone());
                } else {
                    self.finish_admission(plan.ordinal, None)?;
                }
                return Err(flow_error);
            }
        };

        if admission.schema_version != 1 {
            let error = FlowError::new(
                "FlowProtocolError",
                "enqueue-schema-unsupported",
                format!(
                    "enqueue response schema version {} is unsupported",
                    admission.schema_version
                ),
            )
            .at(plan.location)
            .with_ordinal(plan.ordinal)
            .detail("schemaVersion", admission.schema_version);
            self.finish_admission(plan.ordinal, Some(admission.disposition))?;
            return Err(error);
        }

        if admission.disposition != Disposition::Created && admission.payload_hash != expected_hash
        {
            let error = FlowError::new(
                "FlowReplayError",
                "replay-divergence",
                format!(
                    "ordinal {} re-derived payload {} but the ledger recorded {}",
                    plan.ordinal, expected_hash, admission.payload_hash
                ),
            )
            .at(plan.location)
            .with_ordinal(plan.ordinal)
            .detail("expectedHash", expected_hash)
            .detail("recordedHash", admission.payload_hash.clone())
            .detail("expectedLabel", expected_label.unwrap_or_default())
            .detail(
                "recordedLabel",
                admission.recorded_label.clone().unwrap_or_default(),
            );
            self.set_fatal(error.clone());
            return Err(error);
        }

        self.finish_admission(plan.ordinal, Some(admission.disposition))?;
        let mut result = match admission.disposition {
            Disposition::Reused | Disposition::Terminal => {
                admission.terminal.clone().ok_or_else(|| {
                    FlowError::new(
                        "FlowProtocolError",
                        "terminal-result-missing",
                        format!(
                            "{:?} admission for ordinal {} omitted its terminal result",
                            admission.disposition, plan.ordinal
                        ),
                    )
                    .at(plan.location)
                    .with_ordinal(plan.ordinal)
                })?
            }
            Disposition::Created | Disposition::Attached => {
                if let Some(result) = admission.terminal.clone() {
                    result
                } else {
                    self.client
                        .await_terminal(&admission.task_uuid, admission.attempt)
                        .await
                        .map_err(|error| error.into_flow(plan.location, plan.ordinal))?
                }
            }
        };

        result.disposition = admission.disposition;
        validate_terminal_result(&result, &admission, plan.location, plan.ordinal)?;
        if let Some(schema) = &plan.result_schema {
            let validation = result.result.as_ref().map_or_else(
                || {
                    Err(FlowError::new(
                        "FlowResultError",
                        "result-schema-mismatch",
                        "node returned no structured result",
                    )
                    .at(plan.location)
                    .with_ordinal(plan.ordinal))
                },
                |value| {
                    validate_instance(
                        schema,
                        value,
                        "FlowResultError",
                        "result-schema-mismatch",
                        "node result does not match resultSchema",
                        plan.location,
                    )
                    .map_err(|error| error.with_ordinal(plan.ordinal))
                },
            );
            if let Err(error) = validation {
                result.error = Some(NodeFailure {
                    code: error.code.clone(),
                    message: error.message.clone(),
                    details: Some(Value::Object(error.details.clone())),
                });
                self.observe(result.witness_seq, plan.ordinal).await?;
                if plan.settle {
                    return Ok(result);
                }
                return Err(error);
            }
        }

        self.observe(result.witness_seq, plan.ordinal).await?;
        if !result.verdict.is_pass() && !plan.settle {
            return Err(FlowError::new(
                "FlowTerminalError",
                "terminal-failure",
                format!(
                    "node {} completed with verdict {:?}",
                    result.task_uuid, result.verdict
                ),
            )
            .at(plan.location)
            .with_ordinal(plan.ordinal)
            .detail("taskUuid", result.task_uuid.clone())
            .detail(
                "verdict",
                serde_json::to_value(result.verdict).expect("serializing a verdict cannot fail"),
            )
            .detail(
                "node",
                serde_json::to_value(&result).expect("serializing a node result cannot fail"),
            ));
        }
        Ok(result)
    }
}

#[derive(Debug, Clone)]
struct CapturedTrace {
    location: SourceLocation,
    stack: String,
}

struct FlowHooks {
    rejected: RefCell<Vec<JsObject>>,
    root_promises: RefCell<HashSet<JsObject>>,
    rejection_traces: RefCell<HashMap<JsObject, CapturedTrace>>,
}

impl FlowHooks {
    fn new() -> Self {
        Self {
            rejected: RefCell::default(),
            root_promises: RefCell::default(),
            rejection_traces: RefCell::default(),
        }
    }

    fn observe_root(&self, promise: JsObject) {
        self.root_promises.borrow_mut().insert(promise.clone());
        self.rejected
            .borrow_mut()
            .retain(|rejected| rejected != &promise);
    }

    fn unhandled(&self) -> Vec<JsObject> {
        self.rejected.borrow().clone()
    }

    fn rejection_trace(&self, promise: &JsObject) -> Option<CapturedTrace> {
        self.rejection_traces.borrow().get(promise).cloned()
    }
}

impl HostHooks for FlowHooks {
    fn promise_rejection_tracker(
        &self,
        promise: &JsObject,
        operation: OperationType,
        context: &mut Context,
    ) {
        match operation {
            OperationType::Reject => {
                if !self.root_promises.borrow().contains(promise) {
                    let mut rejected = self.rejected.borrow_mut();
                    if !rejected.iter().any(|candidate| candidate == promise) {
                        rejected.push(promise.clone());
                    }
                }
                if let Some(trace) = capture_trace(context) {
                    self.rejection_traces
                        .borrow_mut()
                        .insert(promise.clone(), trace);
                }
            }
            OperationType::Handle => {
                self.rejected
                    .borrow_mut()
                    .retain(|rejected| rejected != promise);
            }
        }
    }

    fn ensure_can_compile_strings(
        &self,
        _realm: Realm,
        _parameters: &[JsString],
        _body: &JsString,
        _direct: bool,
        context: &mut Context,
    ) -> JsResult<()> {
        Err(flow_to_js_error(
            FlowError::determinism(
                "eval",
                "runtime string compilation through eval or Function is forbidden",
                call_site(context),
            ),
            context,
        ))
    }
}

/// Validate and execute one flow script against a daemon client.
pub fn run_script(
    source: &str,
    path: Option<&Path>,
    client: Rc<dyn FlowClient>,
    sink: Rc<dyn LifecycleSink>,
    options: RunOptions,
) -> Result<RunReport, FlowError> {
    if options.flow_run_id.trim().is_empty() {
        return Err(FlowError::new(
            "FlowStartupError",
            "flow-run-id-missing",
            "flowRunId must not be empty",
        )
        .at(RUNTIME_ERROR_LOCATION));
    }
    if options.max_nodes == 0 {
        return Err(FlowError::new(
            "FlowStartupError",
            "max-nodes-invalid",
            "--max-nodes must be positive",
        )
        .at(RUNTIME_ERROR_LOCATION));
    }
    if options.catalog.is_some() != options.catalog_hash.is_some() {
        return Err(FlowError::new(
            "FlowCatalogError",
            "catalog-hash-missing",
            "catalog and catalogHash must be supplied together",
        )
        .at(RUNTIME_ERROR_LOCATION));
    }

    let script_hash = sha256(source.as_bytes());
    let inspection = futures_lite::future::block_on(client.inspect_run(&options.flow_run_id))
        .map_err(|error| error.into_flow(RUNTIME_ERROR_LOCATION, 0))?;
    if let Some(recorded_hash) = inspection.script_hash {
        if recorded_hash != script_hash {
            return Err(FlowError::new(
                "FlowReplayError",
                "script-changed-mid-run",
                format!(
                    "flow run {} is pinned to {recorded_hash}, not {script_hash}",
                    options.flow_run_id
                ),
            )
            .at(RUNTIME_ERROR_LOCATION)
            .detail("recordedHash", recorded_hash)
            .detail("currentHash", script_hash));
        }
    }
    let checked = check_script(
        source,
        path,
        CheckOptions {
            args: Some(&options.args),
            catalog: options.catalog.as_ref(),
            catalog_hash: options.catalog_hash.as_deref(),
        },
    )
    .map_err(|error| error.with_ordinal(0))?;

    let effective_max_nodes = checked
        .meta
        .max_nodes
        .map_or(options.max_nodes, |meta| meta.min(options.max_nodes));
    let shared = Rc::new(HostShared {
        client,
        sink,
        meta: checked.meta.clone(),
        flow_run_id: options.flow_run_id,
        script_hash,
        effective_max_nodes,
        host_call_sites: checked.host_call_sites,
        catalog: options.catalog,
        catalog_hash: options.catalog_hash,
        pool_credentials: options.pool_credentials,
        adapter_skill_revisions: options.adapter_skill_revisions,
        state: RefCell::default(),
    });
    let hooks = Rc::new(FlowHooks::new());
    let executor = Rc::new(FlowJobExecutor::new(shared.clone()));
    let mut context = ContextBuilder::new()
        .host_hooks(hooks.clone())
        .job_executor(executor)
        .build()
        .map_err(|error| {
            FlowError::new(
                "FlowEngineError",
                "engine-initialization",
                format!("cannot initialize Boa: {error}"),
            )
            .at(RUNTIME_ERROR_LOCATION)
        })?;
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(ENGINE_LOOP_LIMIT);
    context
        .runtime_limits_mut()
        .set_recursion_limit(ENGINE_RECURSION_LIMIT);
    context.insert_data(HostHandle {
        shared: shared.clone(),
    });

    harden_engine(&mut context)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    install_host_api(&mut context)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    let args = JsValue::from_json(&options.args, &mut context)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    let meta = JsValue::from_json(&checked.meta_json, &mut context)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    context
        .register_global_property(js_string!("args"), args, Attribute::READONLY)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    context
        .register_global_property(js_string!("flowMeta"), meta, Attribute::READONLY)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;

    let execution = (|| -> Result<Option<Value>, FlowError> {
        evaluate_script(BOOTSTRAP, Some(Path::new(BOOTSTRAP_PATH)), &mut context)
            .map_err(|error| js_error_to_flow(error, &mut context))?;
        let value = evaluate_script(&checked.script_source, path, &mut context)
            .map_err(|error| js_error_to_flow(error, &mut context))?;
        let root_promise = value
            .as_object()
            .and_then(|object| JsPromise::from_object(object).ok());
        if let Some(promise) = &root_promise {
            hooks.observe_root((**promise).clone());
        }

        if let Err(error) = context.run_jobs() {
            if let Some(fatal) = shared.fatal_error() {
                return Err(fatal);
            }
            return Err(js_error_to_flow(error, &mut context));
        }
        if let Some(fatal) = shared.fatal_error() {
            return Err(fatal);
        }
        let final_js = match root_promise {
            Some(promise) => match promise.state() {
                PromiseState::Fulfilled(value) => value,
                PromiseState::Rejected(reason) => {
                    let mut error = js_error_to_flow(JsError::from_opaque(reason), &mut context);
                    if let Some(trace) = hooks.rejection_trace(&promise) {
                        apply_captured_trace(&mut error, trace);
                    }
                    return Err(error);
                }
                PromiseState::Pending => {
                    return Err(FlowError::new(
                        "FlowPromiseError",
                        "promise-pending",
                        "flow script finished with a promise that can never settle",
                    )
                    .at(RUNTIME_ERROR_LOCATION));
                }
            },
            None => value,
        };
        if let Some(promise) = hooks.unhandled().first() {
            let reason = JsPromise::from_object(promise.clone())
                .ok()
                .and_then(|promise| match promise.state() {
                    PromiseState::Rejected(reason) => reason.to_json(&mut context).ok().flatten(),
                    PromiseState::Pending | PromiseState::Fulfilled(_) => None,
                });
            let mut error = FlowError::new(
                "FlowUnhandledRejection",
                "unhandled-rejection",
                "flow script left a rejected promise without a handler",
            )
            .at(RUNTIME_ERROR_LOCATION)
            .detail("reason", reason.unwrap_or(Value::Null));
            if let Some(trace) = hooks.rejection_trace(promise) {
                apply_captured_trace(&mut error, trace);
            }
            return Err(error);
        }
        final_js
            .to_json(&mut context)
            .map_err(|error| js_error_to_flow(error, &mut context))
    })()
    .map_err(|error| shared.annotate_frontier(error));

    let flush = shared.flush_final_logs();
    let final_value = match execution {
        Ok(value) => {
            flush?;
            value
        }
        Err(error) => {
            let _ = flush;
            return Err(error);
        }
    };
    let report = shared.report(final_value);
    shared.sink.emit(json!({
        "type": "flow-completed",
        "flowRunId": report.flow_run_id,
        "flowName": report.flow_name,
        "scriptHash": report.script_hash,
        "ordinals": report.ordinal_keys.len(),
    }))?;
    Ok(report)
}

fn evaluate_script(source: &str, path: Option<&Path>, context: &mut Context) -> JsResult<JsValue> {
    let script = Script::parse(Source::from_reader(source.as_bytes(), path), None, context)?;
    script.evaluate(context)
}

fn harden_engine(context: &mut Context) -> JsResult<()> {
    let global = context.global_object().clone();
    for name in ["Date", "WeakRef", "FinalizationRegistry"] {
        global.delete_property_or_throw(JsString::from(name), context)?;
    }
    let math = global
        .get(js_string!("Math"), context)?
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Math global is not an object"))?;
    let random = boa_engine::object::FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(native_random),
    )
    .name(js_string!("random"))
    .length(0)
    .constructor(false)
    .build();
    math.define_property_or_throw(
        js_string!("random"),
        PropertyDescriptor::builder()
            .value(random)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    Ok(())
}

fn install_host_api(context: &mut Context) -> JsResult<()> {
    for (name, length, function) in [
        ("job", 2, NativeFunction::from_fn_ptr(native_job)),
        ("claude", 2, NativeFunction::from_fn_ptr(native_claude)),
        ("codex", 2, NativeFunction::from_fn_ptr(native_codex)),
        ("local", 2, NativeFunction::from_fn_ptr(native_local)),
        ("sh", 2, NativeFunction::from_fn_ptr(native_sh)),
        ("members", 2, NativeFunction::from_fn_ptr(native_members)),
        ("log", 1, NativeFunction::from_fn_ptr(native_log)),
        (
            "__flowError",
            5,
            NativeFunction::from_fn_ptr(native_error_factory),
        ),
        (
            "__flowLocation",
            0,
            NativeFunction::from_fn_ptr(native_location),
        ),
    ] {
        context.register_global_builtin_callable(JsString::from(name), length, function)?;
    }
    Ok(())
}

fn native_job(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let location = call_site(context);
    let raw = value_to_json(
        args.first().unwrap_or(&JsValue::undefined()),
        "job spec",
        context,
    )?;
    reject_unknown_keys(
        &raw,
        &[
            "argv",
            "adapter",
            "prompt",
            "pools",
            "executor",
            "priority",
            "runtimeMaxSec",
            "evidence",
            "evidenceClass",
            "manifestHash",
            "workspace",
            "brief",
            "key",
            "dedupKey",
            "label",
            "env",
            "resultSchema",
        ],
        location,
    )
    .map_err(|error| flow_to_js_error(error, context))?;
    let spec: NodeSpec = serde_json::from_value(raw).map_err(|error| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("job spec has an invalid shape: {error}"),
            )
            .at(location),
            context,
        )
    })?;
    let settle = settle_option(args.get(1), context)?;
    make_job_promise(spec, NodeRevisions::default(), settle, location, context)
}

fn native_claude(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    native_agent_sugar("claude-code", "claude-window", args, context)
}

fn native_codex(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    native_agent_sugar("codex", "codex-window", args, context)
}

fn native_agent_sugar(
    adapter: &str,
    pool: &str,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    let prompt = required_string(args.first(), "prompt", location, context)?;
    let (mut options, settle) = sugar_options(args.get(1), location, context)?;
    reject_sugar_conflicts(
        &options,
        &["adapter", "pools", "argv", "prompt", "brief"],
        location,
        context,
    )?;
    let revisions = host(context)?.agent_revisions(adapter, &prompt);
    options.insert("adapter".to_owned(), Value::String(adapter.to_owned()));
    options.insert("pools".to_owned(), json!([pool]));
    options.insert("argv".to_owned(), json!([BRIEF_SENTINEL]));
    options.insert("brief".to_owned(), json!({"mission": prompt}));
    let spec = decode_sugar_spec(options, location, context)?;
    make_job_promise(spec, revisions, settle, location, context)
}

fn native_sh(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let location = call_site(context);
    let argv = value_to_json(
        args.first().unwrap_or(&JsValue::undefined()),
        "shell argv",
        context,
    )?;
    let (mut options, settle) = sugar_options(args.get(1), location, context)?;
    reject_sugar_conflicts(&options, &["adapter", "argv", "prompt"], location, context)?;
    options.insert("argv".to_owned(), argv);
    options.insert("adapter".to_owned(), Value::String("shell".to_owned()));
    let spec = decode_sugar_spec(options, location, context)?;
    make_job_promise(spec, NodeRevisions::default(), settle, location, context)
}

fn native_local(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let location = call_site(context);
    let prompt = required_string(args.first(), "prompt", location, context)?;
    let (mut options, settle) = sugar_options(args.get(1), location, context)?;
    reject_sugar_conflicts(
        &options,
        &[
            "adapter",
            "pools",
            "argv",
            "prompt",
            "brief",
            "adapterOptions",
            "selection",
        ],
        location,
        context,
    )?;
    let member_value = options.remove("member").ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowSelectorError",
                "member-required",
                "local(prompt, opts) requires opts.member from members()",
            )
            .at(location),
            context,
        )
    })?;
    let member_id = match &member_value {
        Value::String(id) => id.clone(),
        Value::Object(object) => object
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                flow_to_js_error(
                    FlowError::new(
                        "FlowSelectorError",
                        "member-invalid",
                        "opts.member must be a catalog member object or member id",
                    )
                    .at(location),
                    context,
                )
            })?,
        _ => {
            return Err(flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "member-invalid",
                    "opts.member must be a catalog member object or member id",
                )
                .at(location),
                context,
            ));
        }
    };
    let shared = host(context)?;
    let catalog = shared.catalog.as_ref().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowCatalogError",
                "catalog-required",
                "local() requires a selector catalog",
            )
            .at(location),
            context,
        )
    })?;
    let member = catalog
        .members
        .iter()
        .find(|candidate| candidate.id == member_id)
        .ok_or_else(|| {
            flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "member-unknown",
                    format!("catalog has no member {member_id:?}"),
                )
                .at(location),
                context,
            )
        })?
        .clone();
    let selection_value = member_value
        .as_object()
        .and_then(|object| object.get("selection"))
        .cloned()
        .ok_or_else(|| {
            flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "selection-provenance-missing",
                    "local() member did not come from this run's members() result",
                )
                .at(location),
                context,
            )
        })?;
    let selection: SelectionProvenance =
        serde_json::from_value(selection_value).map_err(|error| {
            flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "selection-provenance-invalid",
                    format!("member selection provenance is invalid: {error}"),
                )
                .at(location),
                context,
            )
        })?;
    if selection.member_id != member.id
        || shared.catalog_hash.as_deref() != Some(selection.catalog_hash.as_str())
        || !shared.selection_was_resolved(&selection)
    {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSelectorError",
                "selection-provenance-invalid",
                "member selection provenance does not match the active catalog",
            )
            .at(location),
            context,
        ));
    }

    let revisions = shared.agent_revisions(&member.adapter, &prompt);
    options.insert("adapter".to_owned(), Value::String(member.adapter));
    options.insert(
        "pools".to_owned(),
        serde_json::to_value(member.pools).expect("a string vector always serializes"),
    );
    options.insert("argv".to_owned(), json!([BRIEF_SENTINEL]));
    options.insert("brief".to_owned(), json!({"mission": prompt}));
    let launch = member.launch.as_object().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowCatalogError",
                "catalog-launch-invalid",
                "catalog member launch must be an object",
            )
            .at(location),
            context,
        )
    })?;
    options.insert("adapterOptions".to_owned(), Value::Object(launch.clone()));
    options.insert(
        "selection".to_owned(),
        serde_json::to_value(selection).expect("selection provenance always serializes"),
    );
    let spec: NodeSpec = serde_json::from_value(Value::Object(options)).map_err(|error| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("local() options have an invalid shape: {error}"),
            )
            .at(location),
            context,
        )
    })?;
    make_job_promise(spec, revisions, settle, location, context)
}

fn native_members(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let location = call_site(context);
    let selector = required_string(args.first(), "selector", location, context)?;
    let opts_value = args.get(1).cloned().unwrap_or_else(JsValue::undefined);
    let options = if opts_value.is_undefined() {
        SelectorOptions::default()
    } else {
        let value = value_to_json(&opts_value, "selector options", context)?;
        serde_json::from_value(value).map_err(|error| {
            flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "selector-invalid-options",
                    format!("members() options are invalid: {error}"),
                )
                .at(location),
                context,
            )
        })?
    };
    let shared = host(context)?;
    if !shared.meta.selectors.iter().any(|item| item == &selector) {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSelectorError",
                "selector-undeclared",
                format!("selector {selector:?} is absent from meta.selectors"),
            )
            .at(location)
            .detail("selector", selector),
            context,
        ));
    }
    let catalog = shared.catalog.as_ref().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowCatalogError",
                "catalog-required",
                "members() requires --catalog",
            )
            .at(location),
            context,
        )
    })?;
    let catalog_hash = shared.catalog_hash.as_deref().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowCatalogError",
                "catalog-hash-missing",
                "members() requires the content hash of its catalog",
            )
            .at(location),
            context,
        )
    })?;
    let selection = resolve_members(catalog, catalog_hash, &selector, &options)
        .map_err(|error| flow_to_js_error(error.at(location), context))?;
    let member_ids = selection
        .members
        .iter()
        .map(|member| member.id.clone())
        .collect::<Vec<_>>();
    shared
        .sink
        .emit(json!({
            "type": "selector-resolved",
            "flowRunId": shared.flow_run_id,
            "selector": selector,
            "opts": options,
            "catalogHash": catalog_hash,
            "members": member_ids,
        }))
        .map_err(|error| flow_to_js_error(error.at(location), context))?;
    shared.record_selection(&selector, catalog_hash, &member_ids);
    let rows = selection
        .members
        .into_iter()
        .map(|member| {
            let provenance = SelectionProvenance {
                selector: selection.selector.clone(),
                catalog_hash: selection.catalog_hash.clone(),
                member_id: member.id.clone(),
                members: member_ids.clone(),
            };
            let mut value =
                serde_json::to_value(member).expect("a catalog member always serializes");
            value
                .as_object_mut()
                .expect("a catalog member serializes to an object")
                .insert(
                    "selection".to_owned(),
                    serde_json::to_value(provenance)
                        .expect("selection provenance always serializes"),
                );
            value
        })
        .collect::<Vec<_>>();
    JsValue::from_json(&Value::Array(rows), context)
}

fn native_log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let location = call_site(context);
    let message = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_json(context)?
        .unwrap_or(Value::Null);
    host(context)?.queue_log(message, location);
    Ok(JsValue::undefined())
}

fn native_random(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    Err(flow_to_js_error(
        FlowError::determinism(
            "Math.random",
            "Math.random is forbidden because it would break replay",
            call_site(context),
        ),
        context,
    ))
}

fn native_error_factory(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let mut location = call_site(context);
    let name = required_string(args.first(), "error name", location, context)?;
    let code = required_string(args.get(1), "error code", location, context)?;
    let message = required_string(args.get(2), "error message", location, context)?;
    let mut error = FlowError::new(name, code, message).at(location);
    if let Some(details) = args.get(3) {
        if let Some(Value::Object(map)) = details.to_json(context)? {
            error.details = map;
        }
    }
    if let Some(position) = args
        .get(4)
        .and_then(|value| value.to_json(context).ok().flatten())
    {
        if let (Some(line), Some(column)) = (
            position.get("line").and_then(Value::as_u64),
            position.get("column").and_then(Value::as_u64),
        ) {
            location = SourceLocation::new(
                u32::try_from(line).unwrap_or(u32::MAX),
                u32::try_from(column).unwrap_or(u32::MAX),
            );
            error.location = Some(location);
        }
    }
    flow_error_value(&error, context)
}

fn native_location(_this: &JsValue, _args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let location = call_site(context);
    JsValue::from_json(
        &json!({"line": location.line, "column": location.column}),
        context,
    )
}

fn make_job_promise(
    spec: NodeSpec,
    revisions: NodeRevisions,
    settle: bool,
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<JsValue> {
    let shared = host(context)?;
    let plan = shared
        .prepare_submission(spec, revisions, settle, location)
        .map_err(|error| flow_to_js_error(error, context))?;
    let promise = JsPromise::from_async_fn(
        async move |context| match shared.execute_submission(plan).await {
            Ok(result) => {
                let value = serde_json::to_value(result).map_err(|error| {
                    JsNativeError::error()
                        .with_message(format!("cannot serialize NodeResult: {error}"))
                })?;
                JsValue::from_json(&value, &mut context.borrow_mut())
            }
            Err(error) => Err(flow_to_js_error(error, &mut context.borrow_mut())),
        },
        context,
    );
    Ok(promise.into())
}

fn sugar_options(
    value: Option<&JsValue>,
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<(Map<String, Value>, bool)> {
    let Some(value) = value else {
        return Ok((Map::new(), false));
    };
    if value.is_undefined() {
        return Ok((Map::new(), false));
    }
    let value = value_to_json(value, "sugar options", context)?;
    let mut options = value.as_object().cloned().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-options",
                "sugar options must be an object",
            )
            .at(location),
            context,
        )
    })?;
    let settle = options
        .remove("settle")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                flow_to_js_error(
                    FlowError::new(
                        "FlowSpecError",
                        "invalid-options",
                        "opts.settle must be boolean",
                    )
                    .at(location),
                    context,
                )
            })
        })
        .transpose()?
        .unwrap_or(false);
    Ok((options, settle))
}

fn decode_sugar_spec(
    options: Map<String, Value>,
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<NodeSpec> {
    reject_unknown_keys(
        &Value::Object(options.clone()),
        &[
            "argv",
            "adapter",
            "pools",
            "executor",
            "priority",
            "runtimeMaxSec",
            "evidence",
            "evidenceClass",
            "manifestHash",
            "workspace",
            "brief",
            "key",
            "dedupKey",
            "label",
            "env",
            "resultSchema",
        ],
        location,
    )
    .map_err(|error| flow_to_js_error(error, context))?;
    serde_json::from_value(Value::Object(options)).map_err(|error| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("sugar options have an invalid shape: {error}"),
            )
            .at(location),
            context,
        )
    })
}

fn reject_sugar_conflicts(
    options: &Map<String, Value>,
    reserved: &[&str],
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<()> {
    if let Some(field) = options
        .keys()
        .find(|field| reserved.contains(&field.as_str()))
    {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "sugar-option-conflict",
                format!("sugar option {field:?} is fixed by its adapter preset"),
            )
            .at(location)
            .detail("field", field.clone()),
            context,
        ));
    }
    Ok(())
}

fn settle_option(value: Option<&JsValue>, context: &mut Context) -> JsResult<bool> {
    let Some(value) = value else {
        return Ok(false);
    };
    if value.is_undefined() {
        return Ok(false);
    }
    let location = call_site(context);
    let value = value_to_json(value, "job options", context)?;
    let object = value.as_object().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-options",
                "job options must be an object",
            )
            .at(location),
            context,
        )
    })?;
    for key in object.keys() {
        if key != "settle" {
            return Err(flow_to_js_error(
                FlowError::new(
                    "FlowSpecError",
                    "invalid-options",
                    format!("unknown job option {key:?}"),
                )
                .at(location),
                context,
            ));
        }
    }
    object
        .get("settle")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                flow_to_js_error(
                    FlowError::new(
                        "FlowSpecError",
                        "invalid-options",
                        "job option settle must be boolean",
                    )
                    .at(location),
                    context,
                )
            })
        })
        .transpose()
        .map(Option::unwrap_or_default)
}

fn validate_node_spec_shape(spec: &NodeSpec, location: SourceLocation) -> Result<(), FlowError> {
    let has_argv = spec.argv.is_some();
    let has_prompt = spec.prompt.is_some();
    if has_argv == has_prompt {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-spec",
            "job spec requires exactly one of argv or adapter+prompt",
        )
        .at(location));
    }
    if has_prompt && spec.adapter.as_deref().is_none_or(str::is_empty) {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-spec",
            "job spec with prompt requires a non-empty adapter",
        )
        .at(location));
    }
    if let Some(argv) = &spec.argv {
        if argv.is_empty()
            || argv.first().is_some_and(String::is_empty)
            || argv.iter().any(|argument| argument.contains('\0'))
        {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-argv",
                "argv requires a non-empty executable and may not contain NUL bytes",
            )
            .at(location));
        }
    }
    for (name, value) in [
        ("adapter", spec.adapter.as_deref()),
        ("executor", spec.executor.as_deref()),
    ] {
        if value.is_some_and(|value| value.is_empty() || value.chars().any(char::is_control)) {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("{name} must be non-empty and contain no control characters"),
            )
            .at(location));
        }
    }
    if spec.runtime_max_sec == Some(0) {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-runtime",
            "runtimeMaxSec must be positive",
        )
        .at(location));
    }
    if spec.brief.as_ref().is_some_and(|brief| !brief.is_object()) {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-brief",
            "brief must be a structured JSON object",
        )
        .at(location));
    }
    if let Some(priority) = &spec.priority {
        if !matches!(priority.as_str(), "interrupt" | "high" | "medium" | "low") {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-priority",
                format!("priority {priority:?} is not a tally priority"),
            )
            .at(location));
        }
    }
    for (name, value) in [
        ("key", spec.key.as_deref()),
        ("dedupKey", spec.dedup_key.as_deref()),
        ("label", spec.label.as_deref()),
    ] {
        if value.is_some_and(|value| value.is_empty() || value.chars().any(char::is_control)) {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("{name} must be non-empty and contain no control characters"),
            )
            .at(location));
        }
    }
    for (name, value) in &spec.env {
        validate_environment_entry(name, value, location)?;
    }
    Ok(())
}

fn normalize_workspace(spec: &mut NodeSpec, location: SourceLocation) -> Result<(), FlowError> {
    let Some(workspace) = spec.workspace.take() else {
        return Ok(());
    };
    let object = workspace.as_object().ok_or_else(|| {
        FlowError::new(
            "FlowSpecError",
            "invalid-workspace",
            "workspace must be an object",
        )
        .at(location)
    })?;
    const FIELDS: [&str; 4] = ["repo", "baseRev", "branch", "worktreePath"];
    if let Some(field) = object
        .keys()
        .find(|field| !FIELDS.contains(&field.as_str()))
    {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-workspace",
            format!("unknown workspace field {field:?}"),
        )
        .at(location)
        .detail("field", field.clone()));
    }
    let mut normalized = Map::new();
    for field in FIELDS {
        let value = object.get(field).and_then(Value::as_str).ok_or_else(|| {
            FlowError::new(
                "FlowSpecError",
                "invalid-workspace",
                format!("workspace.{field} must be a string"),
            )
            .at(location)
        })?;
        if value.trim().is_empty() || value.contains('\0') || value.chars().any(char::is_control) {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-workspace",
                format!("workspace.{field} must be non-empty and contain no control characters"),
            )
            .at(location));
        }
        if field == "worktreePath" && !value.starts_with('/') {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-workspace",
                "workspace.worktreePath must be absolute",
            )
            .at(location));
        }
        normalized.insert(field.to_owned(), Value::String(value.to_owned()));
    }
    spec.workspace = Some(Value::Object(normalized));
    Ok(())
}

fn validate_environment_entry(
    name: &str,
    value: &str,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let mut bytes = name.bytes();
    let valid_name = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if !valid_name || name.starts_with("TALLY_") || name == "CREDENTIALS_DIRECTORY" {
        return Err(FlowError::new(
            "FlowEnvironmentError",
            "reserved-env",
            format!("environment name {name:?} is invalid or reserved"),
        )
        .at(location)
        .detail("name", name.to_owned()));
    }
    if value.contains('\0') {
        return Err(FlowError::new(
            "FlowEnvironmentError",
            "invalid-env-value",
            format!("environment value for {name:?} contains a NUL byte"),
        )
        .at(location)
        .detail("name", name.to_owned()));
    }
    Ok(())
}

fn normalize_prompt(spec: &mut NodeSpec, location: SourceLocation) -> Result<(), FlowError> {
    let Some(prompt) = spec.prompt.take() else {
        return Ok(());
    };
    if prompt.is_empty() {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-prompt",
            "prompt must not be empty",
        )
        .at(location));
    }
    if spec.brief.is_some() {
        return Err(FlowError::new(
            "FlowSpecError",
            "brief-conflict",
            "adapter+prompt cannot also declare brief",
        )
        .at(location));
    }
    spec.argv = Some(vec![BRIEF_SENTINEL.to_owned()]);
    spec.brief = Some(json!({"mission": prompt}));
    Ok(())
}

fn normalize_pools(
    spec: &mut NodeSpec,
    meta: &Meta,
    location: SourceLocation,
) -> Result<(), FlowError> {
    if spec.pools.is_empty() {
        return Err(FlowError::new(
            "FlowPoolError",
            "empty-pools",
            "every flow node must request at least one pool",
        )
        .at(location));
    }
    let mut seen = HashSet::new();
    for pool in &spec.pools {
        if pool.trim().is_empty() || pool.chars().any(char::is_control) {
            return Err(FlowError::new(
                "FlowPoolError",
                "invalid-pool",
                format!("invalid pool name {pool:?}"),
            )
            .at(location));
        }
        if !seen.insert(pool.clone()) {
            return Err(FlowError::new(
                "FlowPoolError",
                "duplicate-pool",
                format!("pool {pool:?} appears more than once"),
            )
            .at(location));
        }
        if !meta.pools.iter().any(|declared| declared == pool) {
            return Err(FlowError::new(
                "FlowPoolError",
                "undeclared-pool",
                format!("pool {pool:?} is absent from meta.pools"),
            )
            .at(location)
            .detail("pool", pool.clone()));
        }
    }
    spec.pools.sort();
    spec.evidence = canonicalize_evidence(&spec.evidence, location)?;
    Ok(())
}

fn canonicalize_evidence(
    evidence: &[String],
    location: SourceLocation,
) -> Result<Vec<String>, FlowError> {
    let invalid = |message: String| {
        FlowError::new("FlowEvidenceError", "invalid-evidence", message).at(location)
    };
    let mut hash_seen = false;
    let mut exit_seen = false;
    let mut canonical = Vec::with_capacity(evidence.len());
    for spec in evidence {
        let (kind, value) = spec
            .split_once(':')
            .ok_or_else(|| invalid(format!("evidence spec must use <kind>:<value>: {spec:?}")))?;
        match kind {
            "artifact" if !value.is_empty() => canonical.push(spec.clone()),
            "artifact" => {
                return Err(invalid("artifact evidence requires a path".to_owned()));
            }
            "hash" => {
                if hash_seen {
                    return Err(invalid("hash evidence appears more than once".to_owned()));
                }
                let (algorithm, expected) = value
                    .split_once(':')
                    .map_or((value, None), |(algorithm, expected)| {
                        (algorithm, Some(expected))
                    });
                if algorithm != "sha256" {
                    return Err(invalid(format!(
                        "unsupported evidence hash algorithm {algorithm:?}"
                    )));
                }
                let rendered = if let Some(expected) = expected {
                    let hex = expected.strip_prefix("sha256:").unwrap_or(expected);
                    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(invalid(
                            "a fixed sha256 value must contain exactly 64 hexadecimal digits"
                                .to_owned(),
                        ));
                    }
                    format!("hash:sha256:{}", hex.to_ascii_lowercase())
                } else {
                    "hash:sha256".to_owned()
                };
                canonical.push(rendered);
                hash_seen = true;
            }
            "exit" => {
                if exit_seen {
                    return Err(invalid("exit evidence appears more than once".to_owned()));
                }
                let code = value.parse::<i32>().map_err(|_| {
                    invalid(format!("exit evidence requires an integer code: {spec:?}"))
                })?;
                if !(0..=255).contains(&code) {
                    return Err(invalid(format!(
                        "exit evidence code must be in 0..=255: {code}"
                    )));
                }
                canonical.push(format!("exit:{code}"));
                exit_seen = true;
            }
            _ => {
                return Err(invalid(format!(
                    "unknown evidence kind {kind:?}; expected artifact, hash, or exit"
                )));
            }
        }
    }
    Ok(canonical)
}

fn normalize_adapter_options(
    spec: &mut NodeSpec,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let mut options = match spec.adapter_options.take() {
        Some(Value::Object(options)) => options,
        Some(_) => {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-adapter-options",
                "adapter options must be an object",
            )
            .at(location));
        }
        None => Map::new(),
    };
    let allowed = [
        "prePromptArgv",
        "environment",
        "approvalPolicy",
        "sandboxPolicy",
        "model",
        "effort",
    ];
    if let Some(field) = options
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            format!("unknown adapter option {field:?}"),
        )
        .at(location)
        .detail("field", field.clone()));
    }

    let pre_prompt_argv = options
        .remove("prePromptArgv")
        .unwrap_or_else(|| Value::Array(Vec::new()));
    if pre_prompt_argv
        .as_array()
        .is_none_or(|items| items.iter().any(|item| !item.is_string()))
    {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            "adapterOptions.prePromptArgv must be an array of strings",
        )
        .at(location));
    }
    let supplied_environment = options
        .remove("environment")
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut environment = supplied_environment.as_object().cloned().ok_or_else(|| {
        FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            "adapterOptions.environment must be an object",
        )
        .at(location)
    })?;
    if environment.values().any(|value| !value.is_string()) {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            "adapterOptions.environment values must be strings",
        )
        .at(location));
    }
    for (name, value) in &environment {
        validate_environment_entry(
            name,
            value
                .as_str()
                .expect("adapter environment values were checked as strings"),
            location,
        )?;
    }
    if pre_prompt_argv
        .as_array()
        .expect("pre-prompt argv was checked as an array")
        .iter()
        .any(|value| {
            value
                .as_str()
                .expect("pre-prompt argv items were checked as strings")
                .contains('\0')
        })
    {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            "adapterOptions.prePromptArgv may not contain NUL bytes",
        )
        .at(location));
    }
    for field in ["approvalPolicy", "sandboxPolicy", "model", "effort"] {
        if options.get(field).is_some_and(|value| !value.is_string()) {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-adapter-options",
                format!("adapterOptions.{field} must be a string"),
            )
            .at(location));
        }
    }
    for (name, value) in std::mem::take(&mut spec.env) {
        if environment
            .insert(name.clone(), Value::String(value))
            .is_some()
        {
            return Err(FlowError::new(
                "FlowSpecError",
                "duplicate-environment",
                format!("environment variable {name:?} is set twice"),
            )
            .at(location));
        }
    }
    let mut environment_entries = environment.into_iter().collect::<Vec<_>>();
    environment_entries.sort_by(|left, right| left.0.cmp(&right.0));
    let environment = environment_entries.into_iter().collect::<Map<_, _>>();
    let mut normalized = Map::new();
    normalized.insert("prePromptArgv".to_owned(), pre_prompt_argv);
    normalized.insert("environment".to_owned(), Value::Object(environment));
    for field in ["approvalPolicy", "sandboxPolicy", "model", "effort"] {
        if let Some(value) = options.remove(field) {
            normalized.insert(field.to_owned(), value);
        }
    }
    spec.adapter_options = Some(Value::Object(normalized));
    Ok(())
}

fn resolve_pool_credentials(
    pools: &[String],
    configured: &BTreeMap<String, BTreeMap<String, PathBuf>>,
) -> BTreeMap<String, PathBuf> {
    let mut credentials = BTreeMap::new();
    for pool in pools {
        if let Some(pool_credentials) = configured.get(pool) {
            for (name, path) in pool_credentials {
                credentials
                    .entry(name.clone())
                    .or_insert_with(|| path.clone());
            }
        }
    }
    credentials
}

fn canonical_payload_hash(
    spec: &NodeSpec,
    credentials: &BTreeMap<String, PathBuf>,
) -> Result<String, FlowError> {
    let mut payload = Map::new();
    if let Some(argv) = &spec.argv {
        payload.insert("argv".to_owned(), json!(argv));
    }
    payload.insert(
        "pool".to_owned(),
        match spec.pools.as_slice() {
            [pool] => Value::String(pool.clone()),
            pools => json!(pools),
        },
    );
    if let Some(executor) = &spec.executor {
        payload.insert("executor".to_owned(), json!(executor));
    }
    if let Some(adapter) = &spec.adapter {
        payload.insert("adapter".to_owned(), json!(adapter));
    }
    if let Some(workspace) = &spec.workspace {
        payload.insert("workspace".to_owned(), workspace.clone());
    }
    payload.insert(
        "adapterOptions".to_owned(),
        spec.adapter_options
            .clone()
            .unwrap_or_else(|| json!({"prePromptArgv": [], "environment": {}})),
    );
    payload.insert("evidence".to_owned(), json!(spec.evidence));
    if let Some(class) = &spec.evidence_class {
        payload.insert("evidenceClass".to_owned(), class.clone());
    }
    if let Some(hash) = &spec.manifest_hash {
        payload.insert("manifestHash".to_owned(), json!(hash));
    }
    if let Some(runtime) = spec.runtime_max_sec {
        payload.insert("runtimeMaxSec".to_owned(), json!(runtime));
    }
    payload.insert("noEnqueue".to_owned(), Value::Bool(true));
    payload.insert(
        "credentials".to_owned(),
        serde_json::to_value(credentials).map_err(|error| {
            FlowError::new(
                "FlowSpecError",
                "payload-serialization",
                format!("cannot serialize resolved pool credentials: {error}"),
            )
        })?,
    );
    if let Some(brief) = &spec.brief {
        let bytes = serde_json::to_vec(brief).map_err(|error| {
            FlowError::new(
                "FlowSpecError",
                "brief-serialization",
                format!("cannot serialize brief: {error}"),
            )
        })?;
        payload.insert("briefHash".to_owned(), Value::String(sha256(&bytes)));
    }
    let bytes = serde_json::to_vec(&Value::Object(payload)).map_err(|error| {
        FlowError::new(
            "FlowSpecError",
            "payload-serialization",
            format!("cannot serialize canonical payload: {error}"),
        )
    })?;
    Ok(sha256(&bytes))
}

fn validate_terminal_result(
    result: &NodeResult,
    admission: &crate::Admission,
    location: SourceLocation,
    ordinal: u64,
) -> Result<(), FlowError> {
    if result.task_uuid != admission.task_uuid {
        return Err(FlowError::new(
            "FlowProtocolError",
            "terminal-task-mismatch",
            format!(
                "terminal result names task {} but admission named {}",
                result.task_uuid, admission.task_uuid
            ),
        )
        .at(location)
        .with_ordinal(ordinal));
    }
    if result.witness_seq == 0 {
        return Err(FlowError::new(
            "FlowProtocolError",
            "terminal-witness-invalid",
            "terminal result witnessSeq must be positive",
        )
        .at(location)
        .with_ordinal(ordinal));
    }
    Ok(())
}

fn reject_unknown_keys(
    value: &Value,
    allowed: &[&str],
    location: SourceLocation,
) -> Result<(), FlowError> {
    let object = value.as_object().ok_or_else(|| {
        FlowError::new(
            "FlowSpecError",
            "invalid-spec",
            "job spec must be an object",
        )
        .at(location)
    })?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(FlowError::new(
                "FlowSpecError",
                "unknown-spec-field",
                format!("unknown job spec field {key:?}"),
            )
            .at(location)
            .detail("field", key.clone()));
        }
    }
    Ok(())
}

fn required_string(
    value: Option<&JsValue>,
    label: &str,
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<String> {
    let Some(value) = value else {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-argument",
                format!("{label} must be a string"),
            )
            .at(location),
            context,
        ));
    };
    let Some(value) = value.as_string() else {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-argument",
                format!("{label} must be a string"),
            )
            .at(location),
            context,
        ));
    };
    Ok(value.to_std_string_escaped())
}

fn value_to_json(value: &JsValue, label: &str, context: &mut Context) -> JsResult<Value> {
    value.to_json(context)?.ok_or_else(|| {
        JsNativeError::typ()
            .with_message(format!("{label} must be JSON-serializable"))
            .into()
    })
}

fn host(context: &Context) -> JsResult<Rc<HostShared>> {
    context
        .get_data::<HostHandle>()
        .map(|handle| handle.shared.clone())
        .ok_or_else(|| {
            JsNativeError::error()
                .with_message("tally-flow host state is unavailable")
                .into()
        })
}

fn call_site(context: &Context) -> SourceLocation {
    let mut fallback = None;
    for frame in context.stack_trace() {
        let position = frame.position();
        let Some(position_value) = position.position else {
            continue;
        };
        let location =
            SourceLocation::new(position_value.line_number(), position_value.column_number());
        fallback.get_or_insert(location);
        if !position.path.to_string().contains(BOOTSTRAP_PATH) {
            return context
                .get_data::<HostHandle>()
                .map_or(location, |handle| handle.shared.exact_call_site(location));
        }
    }
    fallback.unwrap_or(RUNTIME_ERROR_LOCATION)
}

fn capture_trace(context: &Context) -> Option<CapturedTrace> {
    let mut location = None;
    let mut rendered = Vec::new();
    for frame in context.stack_trace() {
        let position = frame.position();
        let Some(source_position) = position.position else {
            continue;
        };
        let path = position.path.to_string();
        if path.contains(BOOTSTRAP_PATH) {
            continue;
        }
        let frame_location = SourceLocation::new(
            source_position.line_number(),
            source_position.column_number(),
        );
        location.get_or_insert(frame_location);
        let function = position.function_name.to_std_string_escaped();
        let function = if function.is_empty() {
            "<anonymous>".to_owned()
        } else {
            function
        };
        rendered.push(format!(
            "    at {function} ({path}:{}:{})",
            frame_location.line, frame_location.column
        ));
    }
    Some(CapturedTrace {
        location: location?,
        stack: rendered.join("\n"),
    })
}

fn apply_captured_trace(error: &mut FlowError, trace: CapturedTrace) {
    if error.location.is_none()
        || (error.location == Some(RUNTIME_ERROR_LOCATION)
            && matches!(
                error.code.as_str(),
                "script-evaluation" | "script-exception" | "unhandled-rejection"
            ))
    {
        error.location = Some(trace.location);
    }
    if error.stack.is_none() && !trace.stack.is_empty() {
        error.stack = Some(trace.stack);
    }
}

fn stack_location(stack: &str) -> Option<SourceLocation> {
    for frame in stack.lines() {
        if !frame.trim_start().starts_with("at ")
            || frame.contains(BOOTSTRAP_PATH)
            || frame.contains("(native at ")
        {
            continue;
        }
        let coordinates = frame.trim().trim_end_matches(')');
        let Some((prefix, column)) = coordinates.rsplit_once(':') else {
            continue;
        };
        let Some((_, line)) = prefix.rsplit_once(':') else {
            continue;
        };
        let (Ok(line), Ok(column)) = (line.parse::<u32>(), column.parse::<u32>()) else {
            continue;
        };
        return Some(SourceLocation::new(line, column));
    }
    None
}

fn flow_error_value(error: &FlowError, context: &mut Context) -> JsResult<JsValue> {
    let value = JsError::from_native(JsNativeError::error().with_message(error.message.clone()))
        .to_opaque(context);
    let object = value
        .as_object()
        .ok_or_else(|| JsNativeError::error().with_message("cannot construct flow error"))?;
    object.set(
        js_string!("name"),
        JsString::from(error.name.as_str()),
        true,
        context,
    )?;
    object.set(
        js_string!("code"),
        JsString::from(error.code.as_str()),
        true,
        context,
    )?;
    if let Some(location) = error.location {
        object.set(
            js_string!("line"),
            JsValue::from(location.line),
            true,
            context,
        )?;
        object.set(
            js_string!("column"),
            JsValue::from(location.column),
            true,
            context,
        )?;
    }
    if let Some(ordinal) = error.ordinal {
        object.set(
            js_string!("ordinal"),
            JsValue::from(ordinal as f64),
            true,
            context,
        )?;
    }
    let details = JsValue::from_json(&Value::Object(error.details.clone()), context)?;
    object.set(js_string!("details"), details, true, context)?;
    Ok(value)
}

fn flow_to_js_error(error: FlowError, context: &mut Context) -> JsError {
    match flow_error_value(&error, context) {
        Ok(value) => JsError::from_opaque(value),
        Err(_) => JsNativeError::error()
            .with_message(format!(
                "{} [{}]: {}",
                error.name, error.code, error.message
            ))
            .into(),
    }
}

fn js_error_to_flow(error: JsError, context: &mut Context) -> FlowError {
    let rendered = error.to_string();
    if error
        .as_native()
        .is_some_and(JsNativeError::is_runtime_limit)
    {
        let location = stack_location(&rendered).unwrap_or(RUNTIME_ERROR_LOCATION);
        return FlowError::new("FlowRuntimeLimitError", "runtime-limit", rendered.clone())
            .at(location)
            .with_stack(rendered);
    }
    let value = error.to_opaque(context);
    if let Some(object) = value.as_object() {
        fn string_property(object: &JsObject, key: &str, context: &mut Context) -> Option<String> {
            object
                .get(JsString::from(key), context)
                .ok()
                .and_then(|value| value.as_string())
                .map(|value| value.to_std_string_escaped())
        }
        fn number_property(object: &JsObject, key: &str, context: &mut Context) -> Option<f64> {
            object
                .get(JsString::from(key), context)
                .ok()
                .and_then(|value| value.as_number())
        }

        let name = string_property(&object, "name", context)
            .unwrap_or_else(|| "FlowScriptError".to_owned());
        let message =
            string_property(&object, "message", context).unwrap_or_else(|| rendered.clone());
        let code =
            string_property(&object, "code", context).unwrap_or_else(|| match name.as_str() {
                "SyntaxError" => "script-syntax".to_owned(),
                "RangeError" => "runtime-limit".to_owned(),
                "ReferenceError" | "TypeError" | "Error" => "script-evaluation".to_owned(),
                _ => "script-exception".to_owned(),
            });
        let explicit_location = number_property(&object, "line", context)
            .zip(number_property(&object, "column", context))
            .and_then(|(line, column)| {
                Some(SourceLocation::new(
                    u32::try_from(line as u64).ok()?,
                    u32::try_from(column as u64).ok()?,
                ))
            });
        let ordinal = number_property(&object, "ordinal", context).map(|value| value as u64);
        let details = object
            .get(js_string!("details"), context)
            .ok()
            .and_then(|value| value.to_json(context).ok().flatten())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let stack = string_property(&object, "stack", context)
            .or_else(|| rendered.contains("\n    at ").then(|| rendered.clone()));
        let location = explicit_location
            .or_else(|| stack.as_deref().and_then(stack_location))
            .unwrap_or(RUNTIME_ERROR_LOCATION);
        return FlowError {
            name,
            code,
            message,
            location: Some(location),
            ordinal,
            details,
            stack,
        };
    }
    let location = stack_location(&rendered).unwrap_or(RUNTIME_ERROR_LOCATION);
    FlowError::new("FlowScriptError", "script-exception", rendered.clone())
        .at(location)
        .with_stack(rendered)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};

    use super::*;
    use crate::{Admission, CatalogMember, ClientError, FlowFuture, RunInspection, Verdict};

    #[derive(Clone)]
    struct Reply {
        disposition: Disposition,
        witness_seq: u64,
        verdict: Verdict,
        result: Option<Value>,
        divergent_hash: bool,
        client_error: Option<ClientError>,
    }

    impl Reply {
        fn pass(disposition: Disposition, witness_seq: u64) -> Self {
            Self {
                disposition,
                witness_seq,
                verdict: Verdict::Pass,
                result: Some(json!({"ok": true})),
                divergent_hash: false,
                client_error: None,
            }
        }

        fn client_error(code: &str) -> Self {
            Self {
                disposition: Disposition::Created,
                witness_seq: 1,
                verdict: Verdict::Failed,
                result: None,
                divergent_hash: false,
                client_error: Some(ClientError::new(code, format!("{code} from mock"))),
            }
        }
    }

    struct MockClient {
        inspection: RunInspection,
        replies: RefCell<VecDeque<Reply>>,
        submissions: RefCell<Vec<FlowSubmission>>,
        terminals: RefCell<BTreeMap<String, NodeResult>>,
        delayed_submissions: RefCell<BTreeSet<usize>>,
    }

    impl MockClient {
        fn new(replies: Vec<Reply>) -> Rc<Self> {
            Rc::new(Self {
                inspection: RunInspection { script_hash: None },
                replies: RefCell::new(replies.into()),
                submissions: RefCell::default(),
                terminals: RefCell::default(),
                delayed_submissions: RefCell::default(),
            })
        }

        fn with_script_hash(hash: &str) -> Rc<Self> {
            Rc::new(Self {
                inspection: RunInspection {
                    script_hash: Some(hash.to_owned()),
                },
                replies: RefCell::default(),
                submissions: RefCell::default(),
                terminals: RefCell::default(),
                delayed_submissions: RefCell::default(),
            })
        }

        fn delay_submission(&self, ordinal: usize) {
            self.delayed_submissions.borrow_mut().insert(ordinal);
        }
    }

    impl FlowClient for MockClient {
        fn inspect_run<'a>(
            &'a self,
            _flow_run_id: &'a str,
        ) -> FlowFuture<'a, Result<RunInspection, ClientError>> {
            Box::pin(std::future::ready(Ok(self.inspection.clone())))
        }

        fn submit<'a>(
            &'a self,
            submission: FlowSubmission,
        ) -> FlowFuture<'a, Result<Admission, ClientError>> {
            let index = self.submissions.borrow().len();
            let reply = self
                .replies
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| Reply::pass(Disposition::Created, (index + 1) as u64));
            if let Some(error) = reply.client_error {
                self.submissions.borrow_mut().push(submission);
                return Box::pin(std::future::ready(Err(error)));
            }
            let task_uuid = format!("task-{index}");
            let terminal = NodeResult {
                task_uuid: task_uuid.clone(),
                verdict: reply.verdict,
                exit_code: Some(if reply.verdict.is_pass() { 0 } else { 1 }),
                witness_seq: reply.witness_seq,
                disposition: reply.disposition,
                result: reply.result,
                gates: None,
                error: (!reply.verdict.is_pass()).then(|| NodeFailure {
                    code: "worker-failed".to_owned(),
                    message: "worker failed".to_owned(),
                    details: None,
                }),
            };
            let inline = matches!(
                reply.disposition,
                Disposition::Reused | Disposition::Terminal
            )
            .then(|| terminal.clone());
            self.terminals
                .borrow_mut()
                .insert(task_uuid.clone(), terminal);
            let payload_hash = if reply.divergent_hash {
                "sha256:divergent".to_owned()
            } else {
                submission.payload_hash.clone()
            };
            self.submissions.borrow_mut().push(submission);
            let mut admission = Some(Ok(Admission {
                schema_version: 1,
                disposition: reply.disposition,
                task_uuid,
                payload_hash,
                attempt: 0,
                terminal: inline,
                recorded_label: None,
                reused_rejected: None,
            }));
            let delayed = self.delayed_submissions.borrow().contains(&index);
            let mut yielded = false;
            Box::pin(std::future::poll_fn(move |cx| {
                if delayed && !yielded {
                    yielded = true;
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                Poll::Ready(
                    admission
                        .take()
                        .expect("mock submission future is not polled after completion"),
                )
            }))
        }

        fn await_terminal<'a>(
            &'a self,
            task_uuid: &'a str,
            _attempt: u32,
        ) -> FlowFuture<'a, Result<NodeResult, ClientError>> {
            let result = self
                .terminals
                .borrow()
                .get(task_uuid)
                .cloned()
                .ok_or_else(|| ClientError::new("missing-terminal", task_uuid));
            Box::pin(std::future::ready(result))
        }
    }

    fn meta(pools: &[&str], selectors: &[&str]) -> String {
        format!(
            "export const meta = {{\n\
             name: 'test-flow',\n\
             description: 'engine test',\n\
             pools: {},\n\
             argsSchema: {{type: 'object'}},\n\
             selectors: {}\n\
             }};\n",
            serde_json::to_string(pools).unwrap(),
            serde_json::to_string(selectors).unwrap(),
        )
    }

    fn run(
        source: &str,
        client: Rc<MockClient>,
    ) -> Result<(RunReport, Rc<VecLifecycleSink>), FlowError> {
        let sink = Rc::new(VecLifecycleSink::default());
        let report = run_script(
            source,
            Some(Path::new("test-flow.js")),
            client,
            sink.clone(),
            RunOptions::new("run-1", json!({})),
        )?;
        Ok((report, sink))
    }

    #[test]
    fn parallel_repeats_an_identical_ordinal_stream_three_times() {
        let source = format!(
            "{}\n(async () => parallel([\n\
             () => sh(['one'], {{pools: ['cpu'], label: 'one'}}),\n\
             () => sh(['two'], {{pools: ['cpu'], label: 'two'}}),\n\
             () => sh(['three'], {{pools: ['cpu'], label: 'three'}})\n\
             ]))()",
            meta(&["cpu"], &[])
        );
        let mut streams = Vec::new();
        for _ in 0..3 {
            let client = MockClient::new(Vec::new());
            let (report, _) = run(&source, client.clone()).unwrap();
            streams.push((
                report.ordinal_keys,
                client
                    .submissions
                    .borrow()
                    .iter()
                    .map(|submission| {
                        (
                            submission.orchestration.node_ordinal,
                            submission.dedup_key.clone(),
                            submission.payload_hash.clone(),
                        )
                    })
                    .collect::<Vec<_>>(),
            ));
        }
        assert_eq!(streams[0], streams[1]);
        assert_eq!(streams[1], streams[2]);
        assert_eq!(
            streams[0].0,
            ["flow:run-1:0", "flow:run-1:1", "flow:run-1:2"]
        );
    }

    #[test]
    fn pipeline_advances_each_item_without_a_stage_barrier() {
        let source = format!(
            "{}\n(async () => pipeline(['a', 'b'],\n\
             (_previous, item) => sh([item, 'stage-1'], {{pools: ['cpu']}}),\n\
             (_previous, item) => sh([item, 'stage-2'], {{pools: ['cpu']}})\n\
             ))()",
            meta(&["cpu"], &[])
        );
        let client = MockClient::new(vec![
            Reply::pass(Disposition::Created, 1),
            Reply::pass(Disposition::Created, 4),
            Reply::pass(Disposition::Created, 2),
            Reply::pass(Disposition::Created, 5),
        ]);
        let (report, _) = run(&source, client.clone()).unwrap();
        assert_eq!(report.observation_order, [1, 2, 4, 5]);
        assert_eq!(
            client
                .submissions
                .borrow()
                .iter()
                .map(|submission| submission.spec.argv.clone().unwrap())
                .collect::<Vec<_>>(),
            [
                ["a", "stage-1"],
                ["b", "stage-1"],
                ["a", "stage-2"],
                ["b", "stage-2"],
            ]
        );
    }

    #[test]
    fn every_disposition_observes_in_witness_order_and_suppresses_prefix_logs() {
        let source = format!(
            "{}\n(async () => {{\n\
             log('frontier-0');\n\
             const values = await parallel([\n\
             () => sh(['zero'], {{pools: ['cpu']}}),\n\
             () => sh(['one'], {{pools: ['cpu']}}),\n\
             () => sh(['two'], {{pools: ['cpu']}}),\n\
             () => sh(['three'], {{pools: ['cpu']}})\n\
             ]);\n\
             log('tail');\n\
             return values.map(value => value.witnessSeq);\n\
             }})()",
            meta(&["cpu"], &[])
        );
        let client = MockClient::new(vec![
            Reply::pass(Disposition::Reused, 30),
            Reply::pass(Disposition::Terminal, 10),
            Reply::pass(Disposition::Attached, 20),
            Reply::pass(Disposition::Created, 40),
        ]);
        client.delay_submission(1);
        let (report, sink) = run(&source, client).unwrap();
        assert_eq!(report.observation_order, [10, 20, 30, 40]);
        assert_eq!(report.final_value, Some(json!([30, 10, 20, 40])));
        let logs = sink
            .events()
            .into_iter()
            .filter(|event| event["type"] == "log")
            .collect::<Vec<_>>();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0]["message"], "tail");
    }

    #[test]
    fn payload_divergence_stops_admission_at_the_mismatched_ordinal() {
        let source = format!(
            "{}\n(async () => parallel([\n\
             () => sh(['zero'], {{pools: ['cpu']}}),\n\
             () => sh(['one'], {{pools: ['cpu']}}),\n\
             () => sh(['two'], {{pools: ['cpu']}})\n\
             ]))()",
            meta(&["cpu"], &[])
        );
        let mut mismatch = Reply::pass(Disposition::Reused, 2);
        mismatch.divergent_hash = true;
        let client = MockClient::new(vec![
            Reply::pass(Disposition::Reused, 1),
            mismatch,
            Reply::pass(Disposition::Created, 3),
        ]);
        let error = run(&source, client.clone()).unwrap_err();
        assert_eq!(error.code, "replay-divergence");
        assert_eq!(error.ordinal, Some(1));
        assert_eq!(client.submissions.borrow().len(), 2);
    }

    #[test]
    fn a_flow_run_refuses_a_changed_script_before_submission() {
        let source = format!("{}\n42;", meta(&["cpu"], &[]));
        let client = MockClient::with_script_hash("sha256:previous-script");
        let error = run(&source, client.clone()).unwrap_err();
        assert_eq!(error.name, "FlowReplayError");
        assert_eq!(error.code, "script-changed-mid-run");
        assert!(error.location.is_some());
        assert!(client.submissions.borrow().is_empty());

        let invalid_edit = "export const meta = {";
        let client = MockClient::with_script_hash("sha256:previous-script");
        let error = run(invalid_edit, client.clone()).unwrap_err();
        assert_eq!(error.code, "script-changed-mid-run");
        assert!(client.submissions.borrow().is_empty());
    }

    #[test]
    fn result_schema_handles_payloads_larger_than_sixty_four_kibibytes() {
        let payload = "x".repeat(70 * 1024);
        let source = format!(
            "{}\n(async () => {{\n\
             const node = await sh(['large'], {{\n\
               pools: ['cpu'],\n\
               resultSchema: {{type: 'object', required: ['payload'], properties: {{payload: {{type: 'string'}}}}}}\n\
             }});\n\
             return node.result.payload.length;\n\
             }})()",
            meta(&["cpu"], &[])
        );
        let mut reply = Reply::pass(Disposition::Created, 1);
        reply.result = Some(json!({"payload": payload}));
        let (report, _) = run(&source, MockClient::new(vec![reply])).unwrap();
        assert_eq!(report.final_value, Some(json!(70 * 1024)));
    }

    #[test]
    fn duplicate_keys_and_result_mismatches_are_typed_with_positions() {
        let duplicate = format!(
            "{}\n(async () => {{\n\
             sh(['a'], {{pools: ['cpu'], key: 'same'}});\n\
             sh(['b'], {{pools: ['cpu'], key: 'same'}});\n\
             }})()",
            meta(&["cpu"], &[])
        );
        let error = run(&duplicate, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.name, "FlowKeyError");
        assert_eq!(error.code, "duplicate-key");
        let duplicate_line = duplicate
            .lines()
            .position(|line| line.contains("sh(['b']"))
            .unwrap();
        let duplicate_column = duplicate
            .lines()
            .nth(duplicate_line)
            .unwrap()
            .find("sh(")
            .unwrap();
        assert_eq!(
            error.location,
            Some(SourceLocation::new(
                duplicate_line as u32 + 1,
                duplicate_column as u32 + 1
            ))
        );

        let mismatch = format!(
            "{}\n(async () => sh(['bad'], {{\n\
             pools: ['cpu'], resultSchema: {{type: 'number'}}\n\
             }}))()",
            meta(&["cpu"], &[])
        );
        let error = run(&mismatch, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.code, "result-schema-mismatch");
        assert!(error.location.is_some());

        let settled = format!(
            "{}\n(async () => {{\n\
             const node = await sh(['bad'], {{\n\
               pools: ['cpu'], resultSchema: {{type: 'number'}}, settle: true\n\
             }});\n\
             return node.error.code;\n\
             }})()",
            meta(&["cpu"], &[])
        );
        let (report, _) = run(&settled, MockClient::new(Vec::new())).unwrap();
        assert_eq!(
            report.final_value,
            Some(Value::String("result-schema-mismatch".to_owned()))
        );
    }

    fn catalog() -> Catalog {
        Catalog {
            version: 1,
            members: ["alpha", "beta", "gamma"]
                .into_iter()
                .enumerate()
                .map(|(index, id)| CatalogMember {
                    id: id.to_owned(),
                    family: format!("family-{index}"),
                    maker: format!("maker-{index}"),
                    classes: vec!["pooled".to_owned()],
                    adapter: "pi".to_owned(),
                    pools: vec!["gpu".to_owned()],
                    launch: json!({"model": id}),
                    architecture: None,
                    fine_tune: None,
                    backend: None,
                    modality: None,
                    role: None,
                    status: None,
                    evidence: None,
                    hosts: Vec::new(),
                    base_checkpoint: None,
                    supersedes: None,
                    superseded_by: None,
                    notes: None,
                })
                .collect(),
        }
    }

    #[test]
    fn selector_quorum_preserves_dissent_and_materializes_one_repair_key() {
        let source = format!(
            "{}\n(async () => {{\n\
             const selected = members('pooled', {{count: 3, diversity: 'maker'}});\n\
             const outcomes = await parallel(selected.map(member => () =>\n\
               local('judge', {{member, settle: true}})\n\
             ), {{settle: true}});\n\
             const attributedRows = outcomes.map((outcome, index) => attributed(selected[index], outcome));\n\
             const q = quorum({{\n\
               results: attributedRows,\n\
               minimumValid: 2,\n\
               requiredMembers: selected.map(member => member.id),\n\
               allowPartial: true\n\
             }});\n\
             const repair = await local('repair', {{member: selected[1], key: repairKey(selected[1])}});\n\
             return {{\n\
               q,\n\
               repair: repair.verdict,\n\
               dissent: dissent({{\n\
                 conclusions: [{{conclusion: 'ship', support: ['alpha', 'gamma'], conflict: ['beta']}}],\n\
                 excluded: [{{memberId: 'beta', reason: 'invalid'}}]\n\
               }})\n\
             }};\n\
             }})()",
            meta(&["gpu"], &["pooled"])
        );
        let client = MockClient::new(vec![
            Reply::pass(Disposition::Created, 1),
            Reply {
                disposition: Disposition::Created,
                witness_seq: 2,
                verdict: Verdict::Failed,
                result: None,
                divergent_hash: false,
                client_error: None,
            },
            Reply::pass(Disposition::Created, 3),
            Reply::pass(Disposition::Created, 4),
        ]);
        let sink = Rc::new(VecLifecycleSink::default());
        let mut options = RunOptions::new("run-1", json!({}));
        options.catalog = Some(catalog());
        options.catalog_hash = Some("sha256:catalog".to_owned());
        let report = run_script(
            &source,
            Some(Path::new("quorum.js")),
            client.clone(),
            sink.clone(),
            options,
        )
        .unwrap();
        assert_eq!(
            report.final_value.as_ref().unwrap()["q"]["valid"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            report.final_value.as_ref().unwrap()["dissent"]["conclusions"][0]["conflict"],
            json!(["beta"])
        );
        assert_eq!(
            client.submissions.borrow()[3].dedup_key,
            "flow:run-1:k:beta@1"
        );
        assert_eq!(
            sink.events()
                .iter()
                .position(|event| event["type"] == "selector-resolved"),
            Some(0)
        );
        assert!(client
            .submissions
            .borrow()
            .iter()
            .take(3)
            .all(|submission| {
                submission.orchestration.selection.is_some()
                    && submission.orchestration.prompt_revision.is_some()
                    && submission.orchestration.skill_revision.is_none()
            }));
    }

    #[test]
    fn every_agent_sugar_carries_revision_and_prompt_only_in_the_structured_brief() {
        let source = format!(
            "{}\n(async () => {{\n\
             const member = members('pooled', {{count: 1}})[0];\n\
             return parallel([\n\
               () => claude('claude mission'),\n\
               () => codex('codex mission'),\n\
               () => local('local mission', {{member}}),\n\
               () => sh(['printf', 'ordinary argv'], {{pools: ['gpu']}})\n\
             ]);\n\
             }})()",
            meta(&["claude-window", "codex-window", "gpu"], &["pooled"])
        );
        let client = MockClient::new(Vec::new());
        let mut options = RunOptions::new("run-1", json!({}));
        options.catalog = Some(catalog());
        options.catalog_hash = Some("sha256:catalog".to_owned());
        options.adapter_skill_revisions = BTreeMap::from([
            ("claude-code".to_owned(), "claude-skill-v2".to_owned()),
            ("codex".to_owned(), "sha256:codex-skill-content".to_owned()),
            ("pi".to_owned(), "local-skill-v4".to_owned()),
        ]);
        run_script(
            &source,
            Some(Path::new("sugar.js")),
            client.clone(),
            Rc::new(VecLifecycleSink::default()),
            options,
        )
        .unwrap();
        let submissions = client.submissions.borrow();
        for (submission, (mission, prompt_revision, skill_revision)) in
            submissions.iter().take(3).zip([
                (
                    "claude mission",
                    "sha256:fb26460aa413216cbc1ff6d4a4f1d248e88b54966fecdb18c220b3cdd46635bb",
                    "claude-skill-v2",
                ),
                (
                    "codex mission",
                    "sha256:ee65191fbc19d66fba6d51c4350e3bfaeaf779afba302312680ef6ea5d1d664a",
                    "sha256:codex-skill-content",
                ),
                (
                    "local mission",
                    "sha256:d07d2dee7383b022e8c9da1cb4767c119a0e31c7b03528a44f5b940524dc985e",
                    "local-skill-v4",
                ),
            ])
        {
            assert_eq!(
                submission.spec.argv.as_deref(),
                Some(&[BRIEF_SENTINEL.to_owned()][..])
            );
            assert_eq!(submission.spec.brief, Some(json!({"mission": mission})));
            assert_eq!(
                submission.orchestration.prompt_revision.as_deref(),
                Some(prompt_revision)
            );
            assert_eq!(
                submission.orchestration.skill_revision.as_deref(),
                Some(skill_revision)
            );
            assert!(!submission
                .spec
                .argv
                .as_ref()
                .unwrap()
                .iter()
                .any(|argument| argument.contains(mission)));
        }
        assert_eq!(
            submissions[3].spec.argv.as_deref(),
            Some(&["printf".to_owned(), "ordinary argv".to_owned()][..])
        );
        assert_eq!(submissions[3].spec.adapter.as_deref(), Some("shell"));
        assert!(submissions[3].spec.brief.is_none());
        assert!(submissions[3].orchestration.prompt_revision.is_none());
        assert!(submissions[3].orchestration.skill_revision.is_none());
    }

    #[test]
    fn resolved_prompt_revision_and_payload_identity_are_replay_stable() {
        let source = format!(
            "{}\n(async () => claude('resolved ' + args.suffix))()",
            meta(&["claude-window"], &[])
        );
        let mut streams = Vec::new();
        for suffix in ["α\n", "α\n", "β\n"] {
            let client = MockClient::new(Vec::new());
            let mut options = RunOptions::new("run-1", json!({"suffix": suffix}));
            options
                .adapter_skill_revisions
                .insert("claude-code".to_owned(), "agent-v3".to_owned());
            run_script(
                &source,
                Some(Path::new("resolved-prompt.js")),
                client.clone(),
                Rc::new(VecLifecycleSink::default()),
                options,
            )
            .unwrap();
            let submission = client.submissions.borrow()[0].clone();
            streams.push((
                submission.orchestration.prompt_revision,
                submission.orchestration.skill_revision,
                submission.payload_hash,
            ));
        }

        assert_eq!(streams[0], streams[1]);
        assert_eq!(
            streams[0].0.as_deref(),
            Some("sha256:100a1b066fe86cc024edd00424d7695640634d2fbf6d5ad195cad42cf9c59a72")
        );
        assert_eq!(streams[0].1.as_deref(), Some("agent-v3"));
        assert_ne!(streams[0].0, streams[2].0);
        assert_ne!(streams[0].2, streams[2].2);
    }

    #[test]
    fn hardening_and_unhandled_rejections_fail_closed() {
        let banned = format!("{}\nMath.random();", meta(&["cpu"], &[]));
        let error = run(&banned, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.name, "FlowDeterminismError");
        assert_eq!(error.code, "determinism-violation");
        assert!(error.location.is_some());

        let dynamic_eval = format!(
            "{}\nconst compile = 'eval'; globalThis[compile]('40 + 2');",
            meta(&["cpu"], &[])
        );
        let error = run(&dynamic_eval, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.name, "FlowDeterminismError");
        assert_eq!(error.code, "determinism-violation");

        let deleted = format!(
            "{}\n({{date: 'Date' in globalThis, weak: 'WeakRef' in globalThis, finalization: 'FinalizationRegistry' in globalThis}});",
            meta(&["cpu"], &[])
        );
        let (report, _) = run(&deleted, MockClient::new(Vec::new())).unwrap();
        assert_eq!(
            report.final_value,
            Some(json!({"date": false, "weak": false, "finalization": false}))
        );

        let runaway = format!(
            "{}\ntry {{ while (true) {{}} }} catch (error) {{ 42; }}",
            meta(&["cpu"], &[])
        );
        let error = run(&runaway, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.name, "FlowRuntimeLimitError");
        assert_eq!(error.code, "runtime-limit");

        let rejected = format!(
            "{}\nPromise.reject(new Error('lost')); 42;",
            meta(&["cpu"], &[])
        );
        let error = run(&rejected, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.name, "FlowUnhandledRejection");
        assert_eq!(error.code, "unhandled-rejection");
        assert!(error.location.is_some());
    }

    #[test]
    fn aggregate_and_loop_errors_keep_their_public_classes_and_call_sites() {
        let aggregate = format!(
            "{}\n(async () => parallel([\n\
             () => sh(['good'], {{pools: ['cpu']}}),\n\
             () => sh(['bad'], {{pools: ['cpu']}})\n\
             ]))()",
            meta(&["cpu"], &[])
        );
        let client = MockClient::new(vec![
            Reply::pass(Disposition::Created, 1),
            Reply {
                disposition: Disposition::Created,
                witness_seq: 2,
                verdict: Verdict::Failed,
                result: None,
                divergent_hash: false,
                client_error: None,
            },
        ]);
        let error = run(&aggregate, client).unwrap_err();
        assert_eq!(error.name, "FlowAggregateError");
        assert_eq!(error.code, "aggregate-failure");
        assert!(error.location.is_some_and(|location| location.line > 1));

        let capped_meta = meta(&["cpu"], &[]).replace("selectors:", "iterationCap: 2,\nselectors:");
        let looped = format!(
            "{capped_meta}\n\
             function launch() {{ return sh(['work'], {{pools: ['cpu']}}); }}\n\
             launch();\n\
             launch();\n\
             launch();"
        );
        let error = run(&looped, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.name, "FlowLoopError");
        assert_eq!(error.code, "iteration-cap");
        assert!(error.location.is_some_and(|location| location.line > 1));
    }

    #[test]
    fn documented_admission_terminal_and_node_cap_rejections_are_typed() {
        let source = format!(
            "{}\n(async () => sh(['work'], {{pools: ['cpu']}}))()",
            meta(&["cpu"], &[])
        );
        for (client_code, name) in [
            ("dedup-key-conflict", "FlowDedupKeyConflict"),
            ("admission-denied", "FlowAdmissionDenied"),
            ("flow-node-cap", "FlowNodeCapError"),
        ] {
            let error = run(
                &source,
                MockClient::new(vec![Reply::client_error(client_code)]),
            )
            .unwrap_err();
            assert_eq!(error.name, name);
            assert_eq!(error.code, client_code);
            assert!(error.location.is_some());
        }

        let failed = Reply {
            disposition: Disposition::Terminal,
            witness_seq: 1,
            verdict: Verdict::Failed,
            result: None,
            divergent_hash: false,
            client_error: None,
        };
        let error = run(&source, MockClient::new(vec![failed])).unwrap_err();
        assert_eq!(error.name, "FlowTerminalError");
        assert_eq!(error.code, "terminal-failure");
        assert!(error.location.is_some());
    }

    #[test]
    fn uncaught_exceptions_keep_stack_position_and_submission_frontier() {
        let source = format!(
            "{}\nfunction fail() {{ throw new Error('boom'); }}\nfail();",
            meta(&[], &[])
        );
        let error = run(&source, MockClient::new(Vec::new())).unwrap_err();
        let throw_line = source
            .lines()
            .position(|line| line.contains("throw new Error"))
            .unwrap() as u32
            + 1;
        assert_eq!(error.code, "script-evaluation");
        assert_eq!(error.location.unwrap().line, throw_line);
        assert_eq!(error.ordinal, Some(0));
        assert!(error
            .stack
            .as_deref()
            .is_some_and(|stack| stack.contains("test-flow.js")));

        let async_source = format!(
            "{}\nasync function fail() {{ throw new Error('async boom'); }}\nfail();",
            meta(&[], &[])
        );
        let async_error = run(&async_source, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(async_error.code, "script-evaluation");
        assert!(async_error
            .location
            .is_some_and(|location| location.line > 1));
        assert_eq!(async_error.ordinal, Some(0));
        assert!(async_error.stack.is_some());
    }

    #[test]
    fn final_frontier_logs_flush_on_failure_and_unhandled_order_is_stable() {
        let source = format!(
            "{}\nlog('tail-before-failure');\nthrow new Error('boom');",
            meta(&[], &[])
        );
        let sink = Rc::new(VecLifecycleSink::default());
        let error = run_script(
            &source,
            Some(Path::new("tail-log.js")),
            MockClient::new(Vec::new()),
            sink.clone(),
            RunOptions::new("run-1", json!({})),
        )
        .unwrap_err();
        assert_eq!(error.code, "script-evaluation");
        assert_eq!(
            sink.events()
                .iter()
                .filter(|event| event["type"] == "log")
                .map(|event| event["message"].clone())
                .collect::<Vec<_>>(),
            [json!("tail-before-failure")]
        );

        let rejected = format!(
            "{}\nPromise.reject('first'); Promise.reject('second'); 42;",
            meta(&[], &[])
        );
        let error = run(&rejected, MockClient::new(Vec::new())).unwrap_err();
        assert_eq!(error.code, "unhandled-rejection");
        assert_eq!(error.details["reason"], "first");
    }

    #[test]
    fn canonical_payload_includes_resolved_nonoptional_defaults() {
        let source = format!(
            "{}\n(async () => sh(['true'], {{pools: ['cpu']}}))()",
            meta(&["cpu"], &[])
        );
        let client = MockClient::new(Vec::new());
        let mut options = RunOptions::new("run-1", json!({}));
        options.pool_credentials.insert(
            "cpu".to_owned(),
            BTreeMap::from([(
                "token".to_owned(),
                PathBuf::from("/run/credentials/cpu-token"),
            )]),
        );
        run_script(
            &source,
            Some(Path::new("test-flow.js")),
            client.clone(),
            Rc::new(VecLifecycleSink::default()),
            options,
        )
        .unwrap();
        let submissions = client.submissions.borrow();
        let submission = &submissions[0];
        assert_eq!(
            submission.spec.adapter_options,
            Some(json!({"prePromptArgv": [], "environment": {}}))
        );
        assert_eq!(
            submission.credentials,
            BTreeMap::from([(
                "token".to_owned(),
                PathBuf::from("/run/credentials/cpu-token")
            )])
        );
        let expected = json!({
            "argv": ["true"],
            "pool": "cpu",
            "adapter": "shell",
            "adapterOptions": {
                "prePromptArgv": [],
                "environment": {}
            },
            "evidence": [],
            "noEnqueue": true,
            "credentials": {
                "token": "/run/credentials/cpu-token"
            }
        });
        assert_eq!(
            submission.payload_hash,
            sha256(&serde_json::to_vec(&expected).unwrap())
        );
    }
}
