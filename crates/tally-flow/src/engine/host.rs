use super::*;

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
    pub microtask_budget: u64,
    pub wall_clock_budget: Duration,
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
            microtask_budget: ENGINE_MICROTASK_LIMIT,
            wall_clock_budget: ENGINE_WALL_CLOCK_LIMIT,
        }
    }
}

#[derive(Debug, Default)]
pub(super) struct NodeRevisions {
    prompt_revision: Option<String>,
    skill_revision: Option<String>,
}

#[derive(Clone, Finalize, JsData, Trace)]
pub(super) struct HostHandle {
    #[unsafe_ignore_trace]
    pub(super) shared: Rc<HostShared>,
}

#[derive(Default)]
pub(super) struct HostState {
    next_ordinal: u64,
    admission_frontier: u64,
    admission_wakers: BTreeMap<u64, Waker>,
    /// Each flow-local key with the ordinal and call site that first claimed it.
    /// The dominant real duplicate is a constant key inside a `.map()`, where the
    /// second use is the same source line, so the ordinal is what separates them.
    explicit_keys: BTreeMap<String, (u64, SourceLocation)>,
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
    pub(super) client: Rc<dyn FlowClient>,
    pub(super) sink: Rc<dyn LifecycleSink>,
    pub(super) meta: Meta,
    pub(super) flow_run_id: String,
    pub(super) script_hash: String,
    pub(super) args_hash: String,
    pub(super) effective_max_nodes: u32,
    pub(super) host_call_sites: Vec<SourceLocation>,
    pub(super) catalog: Option<Catalog>,
    pub(super) catalog_hash: Option<String>,
    pub(super) pool_credentials: BTreeMap<String, BTreeMap<String, PathBuf>>,
    pub(super) adapter_skill_revisions: BTreeMap<String, String>,
    pub(super) state: RefCell<HostState>,
}

impl HostShared {
    pub(crate) fn fatal_error(&self) -> Option<FlowError> {
        self.state.borrow().fatal.clone()
    }

    pub(super) fn set_fatal(&self, error: FlowError) {
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

    pub(super) async fn wait_for_admission(&self, ordinal: u64) -> Result<(), FlowError> {
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

    pub(super) fn finish_admission(
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

    pub(super) async fn observe(&self, witness_seq: u64, ordinal: u64) -> Result<(), FlowError> {
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

    pub(super) fn queue_log(&self, message: Value, location: SourceLocation) {
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

    pub(super) fn record_selection(&self, selector: &str, catalog_hash: &str, members: &[String]) {
        self.state.borrow_mut().resolved_selections.insert((
            selector.to_owned(),
            catalog_hash.to_owned(),
            members.to_vec(),
        ));
    }

    pub(super) fn selection_was_resolved(&self, selection: &SelectionProvenance) -> bool {
        self.state.borrow().resolved_selections.contains(&(
            selection.selector.clone(),
            selection.catalog_hash.clone(),
            selection.members.clone(),
        )) && selection
            .members
            .iter()
            .any(|member| member == &selection.member_id)
    }

    pub(super) fn flush_final_logs(&self) -> Result<(), FlowError> {
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

    pub(super) fn frontier(&self) -> u64 {
        self.state.borrow().next_ordinal
    }

    pub(super) fn annotate_frontier(&self, mut error: FlowError) -> FlowError {
        if error.ordinal.is_none() {
            error.ordinal = Some(self.frontier());
        }
        error
    }

    pub(super) fn exact_call_site(&self, approximate: SourceLocation) -> SourceLocation {
        self.host_call_sites
            .iter()
            .copied()
            .filter(|location| {
                location.line == approximate.line && location.column <= approximate.column
            })
            .max_by_key(|location| location.column)
            .unwrap_or(approximate)
    }

    pub(super) fn report(&self, final_value: Option<Value>) -> RunReport {
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

    pub(super) fn prepare_submission(
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
            if let Some((first_ordinal, first_location)) = state.explicit_keys.get(key) {
                let (first_ordinal, first_location) = (*first_ordinal, *first_location);
                return Err(FlowError::new(
                    "FlowKeyError",
                    "duplicate-key",
                    format!(
                        "flow-local key {key:?} was already claimed by node {first_ordinal} at \
                         line {}, column {}; derive the key from what varies per node",
                        first_location.line, first_location.column
                    ),
                )
                .at(location)
                .with_ordinal(ordinal)
                .detail("key", key.clone())
                .detail("firstOrdinal", first_ordinal)
                .detail(
                    "firstLocation",
                    json!({"line": first_location.line, "column": first_location.column}),
                ));
            }
            state.explicit_keys.insert(key.clone(), (ordinal, location));
            format!("flow:{}:k:{key}", self.flow_run_id)
        } else if let Some(key) = &spec.dedup_key {
            key.clone()
        } else {
            format!("flow:{}:{ordinal}", self.flow_run_id)
        };

        // Keep the host-only schema on the in-process submission so the live
        // client knows whether it must join the daemon's post-ack result
        // projection. canonical_payload_hash() and the wire binding both omit
        // it, preserving the resultSchema boundary.
        let result_schema = spec.result_schema.clone();
        let credentials = resolve_pool_credentials(&spec.pools, &self.pool_credentials);
        let payload_hash = canonical_payload_hash(&spec, &credentials)?;
        let orchestration = Orchestration {
            flow_name: self.meta.name.clone(),
            flow_run_id: self.flow_run_id.clone(),
            script_hash: self.script_hash.clone(),
            args_hash: self.args_hash.clone(),
            catalog_hash: self.catalog_hash.clone(),
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
            task_uuid: spec
                .drv
                .as_ref()
                .map(|_| stable_drv_task_uuid(&self.flow_run_id, ordinal)),
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

    pub(super) fn agent_revisions(&self, adapter: &str, prompt: &str) -> NodeRevisions {
        NodeRevisions {
            prompt_revision: Some(sha256(prompt.as_bytes())),
            skill_revision: self.adapter_skill_revisions.get(adapter).cloned(),
        }
    }

    pub(super) async fn execute_submission(
        &self,
        plan: SubmissionPlan,
    ) -> Result<NodeResult, FlowError> {
        self.wait_for_admission(plan.ordinal).await?;
        let expected_hash = plan.submission.payload_hash.clone();
        let expected_label = plan.submission.spec.label.clone();
        let admission = match self.client.submit(plan.submission).await {
            Ok(admission) => admission,
            Err(error) => {
                let fatal_replay = matches!(
                    error.code.as_str(),
                    "replay-divergence"
                        | "script-changed-mid-run"
                        | "args-changed-mid-run"
                        | "catalog-changed-mid-run"
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

        if admission.payload_hash != expected_hash {
            let error = if admission.disposition == Disposition::Created {
                FlowError::new(
                    "FlowContractError",
                    "payload-hash-contract-drift",
                    format!(
                        "ordinal {} ({}) hashed to {} in the flow runner but {} in the daemon",
                        plan.ordinal,
                        expected_label.as_deref().unwrap_or("<unlabelled>"),
                        expected_hash,
                        admission.payload_hash
                    ),
                )
                .at(plan.location)
                .with_ordinal(plan.ordinal)
                .detail("expectedHash", expected_hash)
                .detail("recordedHash", admission.payload_hash.clone())
                .detail("label", expected_label.unwrap_or_default())
            } else {
                FlowError::new(
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
                )
            };
            self.set_fatal(error.clone());
            return Err(error);
        }

        self.finish_admission(plan.ordinal, Some(admission.disposition))?;
        let mut result = match admission.disposition {
            Disposition::Reused | Disposition::Substituted | Disposition::Terminal => {
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
        if plan.result_schema.is_none()
            && result
                .error
                .as_ref()
                .is_some_and(|error| error.code == "result-projection-timeout")
        {
            result.error = None;
        }
        if let Some(schema) = &plan.result_schema {
            let validation = result.result.as_ref().map_or_else(
                || {
                    let projection_error = result
                        .error
                        .as_ref()
                        .filter(|error| error.code == "result-projection-timeout");
                    let mut error = FlowError::new(
                        "FlowResultError",
                        "result-schema-mismatch",
                        projection_error.map_or("node returned no structured result", |error| {
                            error.message.as_str()
                        }),
                    )
                    .at(plan.location)
                    .with_ordinal(plan.ordinal);
                    if let Some(projection_error) = projection_error {
                        error = error.detail(
                            "projection",
                            serde_json::to_value(projection_error)
                                .expect("node projection failures always serialize"),
                        );
                    }
                    Err(error)
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
        if result.verdict == Verdict::Cancelled && !plan.settle {
            return Err(FlowError::new(
                "FlowCancelledError",
                "flow-cancelled",
                format!("node {} was cancelled", result.task_uuid),
            )
            .at(plan.location)
            .with_ordinal(plan.ordinal)
            .detail("taskUuid", result.task_uuid.clone())
            .detail(
                "node",
                serde_json::to_value(&result).expect("serializing a node result cannot fail"),
            ));
        }
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
