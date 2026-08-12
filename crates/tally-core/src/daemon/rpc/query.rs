use super::super::*;
use crate::query_v2::{
    apply_reader_state_to_jobs, apply_reader_state_to_run, apply_reader_state_to_standup,
    JobsReaderStateMode,
};
use crate::reader_state::{reader_state_path, ReaderState};

/// What one `query.proof` call is asking about.
enum ProofTarget {
    Task(String),
    FlowRun(String),
}

impl DaemonHandler {
    pub(crate) async fn query(
        &self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, WireError> {
        if method == "query.watch" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Params {
                #[serde(default)]
                after: Option<String>,
                #[serde(default)]
                limit: Option<usize>,
            }
            let params: Params = decode_params(params)?;
            return serde_json::to_value(
                self.changes
                    .borrow()
                    .watch(params.after.as_deref(), params.limit)
                    .map_err(change_wire)?,
            )
            .map_err(internal_wire);
        }
        if method == "query.producers" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Params {
                #[serde(default)]
                name: Option<String>,
                #[serde(default)]
                kind: Option<String>,
            }
            let params: Params = decode_params(params)?;
            let (registry, state_dir) = {
                let context = self.context.read().await;
                (
                    context.config.producers.clone(),
                    context.paths.state_dir.clone(),
                )
            };
            return serde_json::to_value(query_producers(
                &registry,
                &state_dir,
                params.name.as_deref(),
                params.kind.as_deref(),
            ))
            .map_err(internal_wire);
        }
        if method == "query.lineage" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields, rename_all = "camelCase")]
            struct Params {
                #[serde(alias = "id", alias = "flow_run")]
                flow_run: String,
            }
            let params: Params = decode_params(params)?;
            // A non-UUID cannot name a flow run, and answering it with a
            // well-formed "not superseded" view is how a mis-rendered ID looks
            // like a normal answer instead of an error. Validate it the way
            // `flow.supersede` does, and canonicalize so that two spellings of
            // one run cannot read as two runs.
            let flow_run = canonical_flow_run_id(&params.flow_run)
                .map_err(|_| WireError::invalid("query lineage run ID must be a UUID"))?;
            // A run with no recorded rollover is still a valid, empty answer
            // rather than a not-found: a supervisor asks this question about
            // every run it is about to replay, including the very first one.
            let view = self.flow_lineage().await?.view(&flow_run);
            return serde_json::to_value(view).map_err(internal_wire);
        }
        if method == "query.storage" {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Params {}
            let _: Params = decode_params(params)?;
            return serde_json::to_value(self.cached_storage()).map_err(internal_wire);
        }

        // Paginated methods decode their parameters first: a continuation
        // cursor is served straight from the page cache without touching the
        // lifecycle history, live jobs, or the witness ledger. Snapshot-cursor
        // semantics already promise that page content is frozen at snapshot
        // time, so continuation pages intentionally skip re-verifying the
        // ledger.
        if method == "query.jobs" {
            return self.query_jobs(params).await;
        }
        if method == "query.log" {
            return self.query_log(params).await;
        }
        if method == "query.trace" {
            return self.query_trace(params).await;
        }

        let QueryProjection {
            history,
            rows,
            details,
            attestations_path,
            live_states,
            live,
            report,
            witness,
            membership,
        } = self.query_projection().await?;

        match method {
            "query.job" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    id: String,
                }
                let params: Params = decode_params(params)?;
                if params.id.trim().is_empty() {
                    return Err(WireError::invalid("query job ID must not be empty"));
                }
                let pool_signals = {
                    let mut context = self.context.write().await;
                    query_pools(&pool_headroom_facts(&mut context)?)
                        .map_err(query_wire)?
                        .pools
                        .into_iter()
                        .map(|pool| (pool.pool, pool.signal))
                        .collect::<BTreeMap<_, _>>()
                };
                let adapters = {
                    let context = self.context.read().await;
                    context.config.adapters.clone()
                };
                let executor = self.executor.clone();
                off_thread(move || {
                    let lanes = trace_lanes(&details, &live, &history);
                    let mut result = query_job_v2(
                        &params.id,
                        &details,
                        &live,
                        &history,
                        &witness,
                        &pool_signals,
                    )
                    .map_err(observability_wire)?;
                    result.job.trace =
                        trace_availability(&result.job.anchor, &lanes, &adapters, &executor);
                    serde_json::to_value(result).map_err(internal_wire)
                })
                .await
            }
            "__campaign.status" => {
                let selector: CampaignStatusSelector = decode_params(params)?;
                if selector.issue_url.trim().is_empty() {
                    return Err(WireError::invalid(
                        "query campaign issueUrl must not be empty",
                    ));
                }
                if selector
                    .registration_id
                    .as_deref()
                    .is_some_and(|value| uuid::Uuid::parse_str(value).is_err())
                {
                    return Err(WireError::invalid(
                        "query campaign registrationId must be a UUID",
                    ));
                }
                if selector
                    .latest_observation
                    .as_deref()
                    .is_some_and(|value| value.trim().is_empty())
                {
                    return Err(WireError::invalid(
                        "query campaign latestObservation must not be empty",
                    ));
                }
                off_thread(move || {
                    let (ledger_verified, attestations) =
                        read_attestations_advisory(&attestations_path);
                    serde_json::to_value(
                        query_campaign_status(
                            &selector,
                            &details,
                            &live,
                            &history,
                            &witness,
                            Utc::now(),
                            &membership,
                            &AttestationEvidence::new(ledger_verified, &attestations),
                        )
                        .map_err(observability_wire)?,
                    )
                    .map_err(internal_wire)
                })
                .await
            }
            "query.run" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    id: String,
                }
                let params: Params = decode_params(params)?;
                if params.id.trim().is_empty() {
                    return Err(WireError::invalid("query run ID must not be empty"));
                }
                let lineage = self.flow_lineage().await?;
                let reader_state = self.reader_state_advisory().await;
                let executor = self.executor.clone();
                off_thread(move || {
                    // Not read until the id is known to resolve (#404). The
                    // chain is parsed and hash-verified end to end on every
                    // read -- ~2.7 ms/MB, projecting ~120 ms per call at this
                    // repo's own completion count -- and an id that does not
                    // resolve never reaches the rollup that consumes it.
                    // `query_run` raises its `UnknownJob` from the same
                    // predicate, so it, not this, is still what answers;
                    // skipping the read cannot change what comes back.
                    let (ledger_verified, attestations) = if flow_run_exists(
                        &params.id,
                        &details,
                        &live,
                        &history,
                        &witness,
                        &membership,
                    ) {
                        read_attestations_advisory(&attestations_path)
                    } else {
                        (false, Vec::new())
                    };
                    let mut result = query_run(
                        &params.id,
                        &details,
                        &live,
                        &history,
                        &witness,
                        Utc::now(),
                        &membership,
                        &AttestationEvidence::new(ledger_verified, &attestations),
                    )
                    .map_err(observability_wire)?;
                    apply_run_lineage(&mut result, &lineage);
                    apply_campaign_run_supersession(
                        &mut result,
                        &details,
                        &history,
                        &witness,
                        &membership,
                    );
                    apply_reader_state_to_run(&mut result, &reader_state);
                    for failure in &mut result.failures {
                        let (Some(attempt), Some(lease_epoch), Ok(uuid)) = (
                            failure.attempt,
                            failure.lease_epoch,
                            Uuid::parse_str(&failure.task_uuid),
                        ) else {
                            continue;
                        };
                        let identity = ExecutionIdentity {
                            job_id: uuid,
                            task_uuid: Some(uuid),
                            task_ref: failure.task_ref.clone(),
                        };
                        let Ok(Some(paths)) =
                            executor.retained_capture_paths(&identity, attempt, lease_epoch)
                        else {
                            continue;
                        };
                        let Some(path) = paths.failure_stderr.as_ref() else {
                            continue;
                        };
                        failure.capture_path = Some(path.display().to_string());
                        if failure.stderr_tail.is_none() {
                            if let Ok(excerpt) = crate::executor::read_capture_excerpt(path) {
                                failure.stderr_tail = Some(excerpt.text);
                                failure.stderr_truncated = Some(excerpt.truncated);
                            }
                        }
                    }
                    serde_json::to_value(result).map_err(internal_wire)
                })
                .await
            }
            "query.status" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    #[serde(default)]
                    pool: Option<String>,
                }
                let params: Params = decode_params(params)?;
                let pools = {
                    let mut context = self.context.write().await;
                    pool_headroom_facts(&mut context)?
                };
                let storage = self.cached_storage();
                off_thread(move || {
                    let journal = journal_entries(&history);
                    let mut view =
                        query_status(&pools, params.pool.as_deref(), &rows, &journal, &witness)
                            .map_err(query_wire)?;
                    overlay_live_states(&mut view.jobs, &live_states);
                    let mut value = serde_json::to_value(view).map_err(internal_wire)?;
                    value["storage"] = serde_json::to_value(storage).map_err(internal_wire)?;
                    Ok(value)
                })
                .await
            }
            "query.proof" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields, rename_all = "camelCase")]
                struct Params {
                    #[serde(default)]
                    task: Option<String>,
                    #[serde(default)]
                    flow_run: Option<String>,
                    #[serde(default)]
                    attempt: Option<u32>,
                }
                let params: Params = decode_params(params)?;
                let target = match (
                    params
                        .task
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty()),
                    params
                        .flow_run
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty()),
                ) {
                    (Some(_), Some(_)) => {
                        return Err(WireError::invalid(
                            "query proof takes either a task or a flow run, not both",
                        ));
                    }
                    (Some(task), None) => ProofTarget::Task(task.to_owned()),
                    (None, Some(flow_run)) => {
                        if params.attempt.is_some() {
                            return Err(WireError::invalid(
                                "query proof --attempt applies to a single task, not a flow run",
                            ));
                        }
                        ProofTarget::FlowRun(flow_run.to_owned())
                    }
                    (None, None) => {
                        return Err(WireError::invalid(
                            "query proof requires a task or a flow run",
                        ));
                    }
                };
                off_thread(move || {
                    let (attestation_report, attestations) = read_attestations(&attestations_path)?;
                    if !attestation_report.ok {
                        return Err(internal_wire(
                            "attestation verification failed during proof query",
                        ));
                    }
                    match target {
                        ProofTarget::Task(task) => serde_json::to_value(
                            query_proof(
                                &task,
                                params.attempt,
                                &details,
                                &history,
                                &report,
                                &witness,
                                &attestations,
                            )
                            .map_err(observability_wire)?,
                        )
                        .map_err(internal_wire),
                        ProofTarget::FlowRun(flow_run) => serde_json::to_value(
                            query_flow_proofs(
                                &flow_run,
                                &details,
                                &history,
                                &report,
                                &witness,
                                &attestations,
                                &membership,
                            )
                            .map_err(observability_wire)?,
                        )
                        .map_err(internal_wire),
                    }
                })
                .await
            }
            "query.producers" | "query.watch" | "query.jobs" | "query.log" | "query.trace" => {
                unreachable!("early read-only query paths return before projection setup")
            }
            "query.render" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    #[serde(default)]
                    format: Option<String>,
                    #[serde(default)]
                    scope: RenderScope,
                }
                let params: Params = decode_params(params)?;
                if params
                    .format
                    .as_deref()
                    .is_some_and(|format| !matches!(format, "text" | "json"))
                {
                    return Err(WireError::invalid("format must be text or json"));
                }
                off_thread(move || {
                    let journal = journal_entries(&history);
                    let mut view = query_render(params.scope, &rows, &journal, &witness);
                    overlay_live_states(&mut view.jobs, &live_states);
                    if params.format.as_deref() == Some("text") {
                        serde_json::to_string_pretty(&view)
                            .map(Value::String)
                            .map_err(internal_wire)
                    } else {
                        serde_json::to_value(view).map_err(internal_wire)
                    }
                })
                .await
            }
            "query.standup" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {
                    #[serde(default)]
                    since: Option<String>,
                    #[serde(default)]
                    source: Option<String>,
                    /// Include entries and runs archived as operator
                    /// reader-state. Default `false` hides them.
                    #[serde(default)]
                    archived: bool,
                }
                let params: Params = decode_params(params)?;
                let include_archived = params.archived;
                let since_realtime_us = params
                    .since
                    .as_deref()
                    .map(|since| {
                        chrono::DateTime::parse_from_rfc3339(since)
                            .map_err(|_| {
                                WireError::invalid(format!("invalid since timestamp {since:?}"))
                            })
                            .and_then(|timestamp| {
                                u64::try_from(timestamp.timestamp_micros()).map_err(|_| {
                                    WireError::invalid("since timestamp predates the Unix epoch")
                                })
                            })
                    })
                    .transpose()?;
                let until = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
                let reader_state = self.reader_state_advisory().await;
                off_thread(move || {
                    let journal = journal_entries(&history);
                    let mut digest = query_standup(
                        &rows,
                        &journal,
                        &witness,
                        &StandupOptions {
                            since: params.since,
                            since_realtime_us,
                            until,
                            source: params.source,
                        },
                    );
                    for entry in &mut digest.in_flight {
                        if let Some(state) = entry
                            .task_uuid
                            .as_ref()
                            .and_then(|task_uuid| live_states.get(task_uuid))
                        {
                            entry.state.clone_from(state);
                        }
                    }
                    // Same deferral as `query.run` (#404): a window that touched
                    // no flow run has nothing to roll up, and the chain read is a
                    // full parse and hash-verify. `apply_standup_usage` computes
                    // the touched set from this same function, so it cannot find
                    // work this skipped the evidence for.
                    let (ledger_verified, attestations) =
                        if standup_touched_runs(&digest, &details, &membership).is_empty() {
                            (false, Vec::new())
                        } else {
                            read_attestations_advisory(&attestations_path)
                        };
                    apply_standup_usage(
                        &mut digest,
                        &details,
                        &witness,
                        &membership,
                        &AttestationEvidence::new(ledger_verified, &attestations),
                    );
                    apply_reader_state_to_standup(
                        &mut digest,
                        &details,
                        &witness,
                        &reader_state,
                        include_archived,
                    );
                    serde_json::to_value(digest).map_err(internal_wire)
                })
                .await
            }
            "query.pools" => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Params {}
                let _: Params = decode_params(params)?;
                let pools = {
                    let mut context = self.context.write().await;
                    pool_headroom_facts(&mut context)?
                };
                serde_json::to_value(query_pools(&pools).map_err(query_wire)?)
                    .map_err(internal_wire)
            }
            _ => unreachable!("query methods are filtered by the RPC dispatcher"),
        }
    }
}

/// Run one fresh query's corpus-scale construction on the blocking pool.
///
/// The daemon's runtime is a single thread: every `select!` arm and every RPC
/// connection share it, so query construction that scales with the durable
/// corpus used to be time the dispatch loop could not re-enter its select —
/// at ~30k durable rows, minutes of it (#431). The closure receives only
/// immutable snapshots taken under the context lock, so it computes over one
/// consistent frozen view: admission, scheduling, and completion proceed on
/// the dispatch thread while it runs, and a mutation that lands after the
/// snapshot is simply not visible to this answer — never partially visible.
async fn off_thread<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, WireError> + Send + 'static,
) -> Result<T, WireError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| internal_wire(format!("query construction worker failed: {error}")))?
}

/// Read and verify the advisory attestation chain.
///
/// Called from inside [`off_thread`] closures, so the parse and hash-verify
/// already run on the blocking pool. The verification report travels back with
/// the records rather than being consumed here: `query.proof` fails closed on
/// a chain that did not verify, while the usage rollup carries the same fact
/// onto the wire, because a rollup that answered an unverified ledger with a
/// confident zero would be the wrong-evidence defect this whole surface exists
/// to avoid.
fn read_attestations(
    path: &Path,
) -> Result<(AttestationVerifyReport, Vec<AttestationRecord>), WireError> {
    read_verified_attestations(path).map_err(internal_wire)
}

/// The same read, for the callers whose answer must survive an unreadable
/// advisory chain.
///
/// A corrupt chain already degrades gracefully — `read_verified_attestations`
/// reports `ok: false` and returns no records — but an **I/O** failure is an
/// `Err`, and propagating it would take down the whole canonical run view
/// (task table, current nodes, failures, anomalies) because a permissions or
/// disk fault hit an advisory artifact. Availability of the canonical view must
/// not depend on advisory-chain health; the rollup degrades to
/// [`AttestationEvidence::unavailable`], which is the state that type exists
/// for, and says so in its own caveats.
fn read_attestations_advisory(path: &Path) -> (bool, Vec<AttestationRecord>) {
    match read_attestations(path) {
        Ok((report, records)) => (report.ok, records),
        Err(error) => {
            eprintln!(
                "tally: advisory attestation ledger unreadable for a usage rollup: {}",
                error.message
            );
            (false, Vec::new())
        }
    }
}

/// The journal projection of a lifecycle snapshot, one entry per record.
///
/// Built inside [`off_thread`] closures by the consumers that need it: it
/// clones every record's fields, which is exactly the O(all lifecycle records)
/// copying that must not run on the dispatch thread (#431).
fn journal_entries(history: &crate::history::LifecycleSnapshot) -> Vec<JournalEntry> {
    history
        .records
        .iter()
        .map(|record| JournalEntry {
            fields: record.fields.clone(),
            realtime_us: Some(record.realtime_us),
        })
        .collect()
}

// The shared read-only projection every fresh (non-continuation) query
// envelope is built from. Assembling it re-verifies the witness ledger, so
// continuation pages never construct one. Every field is Send and cheap to
// move: the corpus-scale consumers run on the blocking pool, and what this
// assembly leaves on the dispatch thread is amortized O(live jobs) per query
// plus `Arc` clones — with one O(corpus) snapshot-cache rebuild per mutation,
// paid by the first query after it (#431).
pub(crate) struct QueryProjection {
    pub(crate) history: std::sync::Arc<crate::history::LifecycleSnapshot>,
    pub(crate) rows: std::sync::Arc<Vec<RowFact>>,
    pub(crate) details: std::sync::Arc<Vec<RowDetailFact>>,
    pub(crate) attestations_path: PathBuf,
    pub(crate) live_states: HashMap<String, String>,
    pub(crate) live: Vec<LiveJobFact>,
    pub(crate) report: crate::witness::VerifyReport,
    pub(crate) witness: std::sync::Arc<Vec<WitnessRecord>>,
    /// Durable run membership, so that a `flowRun`-scoped projection resolves
    /// the nodes a run was handed and not only the ones whose row it owns.
    pub(crate) membership: Arc<FlowMembership>,
}

impl DaemonHandler {
    pub(crate) async fn query_projection(&self) -> Result<QueryProjection, WireError> {
        let membership = self.flow_membership().await?;
        let history = self.history.borrow_mut().shared_snapshot();
        let (rows, details, attestations_path, live_states, live, report, witness) = {
            let mut context = self.context.write().await;
            // The cached view verifies only bytes appended since the last
            // read; a broken suffix or shrunken ledger fails the query.
            let witness = context.witness_view.records().map_err(internal_wire)?;
            let report = context.witness_view.report();
            (
                context.query_rows.snapshot(),
                context.query_details.snapshot(),
                context.paths.attestations_path(),
                context
                    .jobs
                    .values()
                    .filter(|job| job.state != JobState::Completed)
                    .map(|job| (job.stable_key(), state_name(job.state).to_owned()))
                    .collect::<HashMap<_, _>>(),
                context
                    .jobs
                    .values()
                    .filter(|job| job.state != JobState::Completed)
                    .map(|job| LiveJobFact {
                        anchor: job.stable_key(),
                        job_id: job.job_id.to_string(),
                        live_state: state_name(job.state).to_owned(),
                        attempt: job.row.attempt,
                        lease_epoch: job.row.lease_epoch,
                        unit: job.identity().unit_name(),
                        labor_class: job.labor_class,
                    })
                    .collect::<Vec<_>>(),
                report,
                witness,
            )
        };
        Ok(QueryProjection {
            history,
            rows,
            details,
            attestations_path,
            live_states,
            live,
            report,
            witness,
            membership,
        })
    }

    async fn query_jobs(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Params {
            #[serde(default, alias = "state")]
            live_state: Option<String>,
            #[serde(default, alias = "verdict")]
            terminal_verdict: Option<Verdict>,
            #[serde(default)]
            pool: Option<String>,
            #[serde(default)]
            executor: Option<String>,
            #[serde(default)]
            adapter: Option<String>,
            #[serde(default)]
            source: Option<String>,
            #[serde(default)]
            origin: Option<String>,
            #[serde(default)]
            parent: Option<String>,
            #[serde(default)]
            flow_run: Option<String>,
            #[serde(default)]
            session: Option<String>,
            #[serde(default)]
            since: Option<String>,
            #[serde(default)]
            until: Option<String>,
            #[serde(default)]
            limit: Option<usize>,
            #[serde(default)]
            cursor: Option<String>,
            /// Include jobs whose creating run is archived operator
            /// reader-state. The default (`false`) hides them, so this must
            /// be part of the fingerprint below: two calls that differ only
            /// here must never share a cached page.
            #[serde(default)]
            archived: bool,
        }
        let params: Params = decode_params(params)?;
        let fingerprint = serde_json::to_string(&json!({
            "liveState": params.live_state.clone(),
            "terminalVerdict": params.terminal_verdict,
            "pool": params.pool.clone(),
            "executor": params.executor.clone(),
            "adapter": params.adapter.clone(),
            "source": params.source.clone(),
            "origin": params.origin.clone(),
            "parent": params.parent.clone(),
            "flowRun": params.flow_run.clone(),
            "session": params.session.clone(),
            "since": params.since.clone(),
            "until": params.until.clone(),
            "archived": params.archived,
        }))
        .map_err(internal_wire)?;
        if let Some(cursor) = params.cursor.as_deref() {
            return self
                .pages
                .borrow_mut()
                .page("query.jobs", &fingerprint, params.limit, Some(cursor), None)
                .map_err(pagination_wire);
        }
        let QueryProjection {
            history,
            details,
            live,
            witness,
            membership,
            ..
        } = self.query_projection().await?;
        let pool_signals = {
            let mut context = self.context.write().await;
            query_pools(&pool_headroom_facts(&mut context)?)
                .map_err(query_wire)?
                .pools
                .into_iter()
                .map(|pool| (pool.pool, pool.signal))
                .collect::<BTreeMap<_, _>>()
        };
        let adapters = {
            let context = self.context.read().await;
            context.config.adapters.clone()
        };
        let reader_state = self.reader_state_advisory().await;
        let executor = self.executor.clone();
        let limit = params.limit;
        let explicit_flow_run = params.flow_run.clone();
        let envelope = off_thread(move || {
            let lanes = trace_lanes(&details, &live, &history);
            // Grouped once: resolving the lane set through the public
            // `trace_availability` scan once per item made this collection
            // quadratic in the corpus (#431).
            let mut lanes_by_anchor = HashMap::<&str, Vec<&TraceLane>>::new();
            for lane in &lanes {
                lanes_by_anchor
                    .entry(lane.task_uuid.as_str())
                    .or_default()
                    .push(lane);
            }
            let mut result = query_jobs_v2(
                &details,
                &live,
                &history,
                &witness,
                &pool_signals,
                &JobsFilter {
                    live_state: params.live_state,
                    terminal_verdict: params.terminal_verdict,
                    pool: params.pool,
                    executor: params.executor,
                    adapter: params.adapter,
                    source: params.source,
                    origin: params.origin,
                    parent: params.parent,
                    flow_run: params.flow_run,
                    session: params.session,
                    since: params.since,
                    until: params.until,
                },
                &membership,
            )
            .map_err(observability_wire)?;
            for item in &mut result.items {
                item.trace = anchor_trace_availability(
                    lanes_by_anchor
                        .get(item.anchor.as_str())
                        .cloned()
                        .unwrap_or_default(),
                    &adapters,
                    &executor,
                );
            }
            let reader_state_mode = match explicit_flow_run.as_deref() {
                Some(flow_run) => JobsReaderStateMode::ExplicitLookup { flow_run },
                None => JobsReaderStateMode::Broad {
                    include_archived: params.archived,
                },
            };
            apply_reader_state_to_jobs(&mut result.items, &reader_state, reader_state_mode);
            let envelope = serde_json::to_value(result).map_err(internal_wire)?;
            crate::pagination::prepare_snapshot(envelope).map_err(pagination_wire)
        })
        .await?;
        self.pages
            .borrow_mut()
            .page_prepared("query.jobs", &fingerprint, limit, envelope)
            .map_err(pagination_wire)
    }

    async fn query_log(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Params {
            #[serde(default)]
            task: Option<String>,
            #[serde(default)]
            flow_run: Option<String>,
            #[serde(default)]
            attempt: Option<u32>,
            #[serde(default)]
            session: Option<String>,
            #[serde(default)]
            event: Option<TallyEvent>,
            #[serde(default)]
            source: Option<String>,
            #[serde(default)]
            since: Option<String>,
            #[serde(default)]
            until: Option<String>,
            #[serde(default)]
            limit: Option<usize>,
            #[serde(default)]
            cursor: Option<String>,
            /// Durable lifecycle-stream position. Distinct from `since`, which
            /// is a wall-clock time filter, and from `cursor`, which is an
            /// ephemeral page offset.
            #[serde(default)]
            after: Option<String>,
            #[serde(default)]
            provenance: Option<bool>,
        }
        let params: Params = decode_params(params)?;
        let fingerprint = serde_json::to_string(&json!({
            "task": params.task.clone(),
            "flowRun": params.flow_run.clone(),
            "attempt": params.attempt,
            "session": params.session.clone(),
            "event": params.event,
            "source": params.source.clone(),
            "since": params.since.clone(),
            "until": params.until.clone(),
            "after": params.after.clone(),
            "provenance": params.provenance,
        }))
        .map_err(internal_wire)?;
        if let Some(cursor) = params.cursor.as_deref() {
            return self
                .pages
                .borrow_mut()
                .page("query.log", &fingerprint, params.limit, Some(cursor), None)
                .map_err(pagination_wire);
        }
        let QueryProjection {
            details,
            history,
            witness,
            membership,
            ..
        } = self.query_projection().await?;
        let explicit_event = params.event.is_some();
        let limit = params.limit;
        let envelope = off_thread(move || {
            let mut result = query_lifecycle_log(
                &details,
                &history,
                &witness,
                &LifecycleLogFilter {
                    task: params.task,
                    flow_run: params.flow_run,
                    attempt: params.attempt,
                    session: params.session,
                    event: params.event,
                    source: params.source,
                    since: params.since,
                    until: params.until,
                },
                &membership,
            )
            .map_err(observability_wire)?;
            if params.provenance == Some(false) {
                result = collapse_lifecycle_echoes(result, !explicit_event);
            }
            // The durable position is applied after echo collapse so a terminal
            // transition is still merged with its witness before the window is
            // narrowed; collapse semantics stay exactly as they were.
            let head = log_position_head(&history, &witness);
            if let Some(after) = params.after.as_deref() {
                let after = LogPosition::parse(after).map_err(observability_wire)?;
                result.items.retain(|item| after.precedes(&item.cursor));
                let floor = log_position_floor(&history, &witness);
                if after.lifecycle < floor.lifecycle || after.witness < floor.witness {
                    result.position_gap = Some(PositionGap {
                        requested: after.render(),
                        earliest_available: floor.render(),
                    });
                }
            }
            result.position = Some(head.render());
            let envelope = serde_json::to_value(result).map_err(internal_wire)?;
            crate::pagination::prepare_snapshot(envelope).map_err(pagination_wire)
        })
        .await?;
        self.pages
            .borrow_mut()
            .page_prepared("query.log", &fingerprint, limit, envelope)
            .map_err(pagination_wire)
    }

    async fn query_trace(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            task: String,
            #[serde(default)]
            attempt: Option<u32>,
            #[serde(default)]
            limit: Option<usize>,
            #[serde(default)]
            cursor: Option<String>,
        }
        let params: Params = decode_params(params)?;
        if params.task.trim().is_empty() {
            return Err(WireError::invalid("query trace task must not be empty"));
        }
        let fingerprint = serde_json::to_string(&json!({
            "task": params.task.clone(),
            "attempt": params.attempt,
        }))
        .map_err(internal_wire)?;
        if let Some(cursor) = params.cursor.as_deref() {
            return self
                .pages
                .borrow_mut()
                .page(
                    "query.trace",
                    &fingerprint,
                    params.limit,
                    Some(cursor),
                    None,
                )
                .map_err(pagination_wire);
        }
        let QueryProjection {
            history,
            details,
            live,
            witness,
            ..
        } = self.query_projection().await?;
        let adapters = {
            let context = self.context.read().await;
            context.config.adapters.clone()
        };
        let executor = self.executor.clone();
        let limit = params.limit;
        let envelope = off_thread(move || {
            let lanes = trace_lanes(&details, &live, &history);
            let envelope = serde_json::to_value(
                query_trace(
                    &params.task,
                    params.attempt,
                    &lanes,
                    &adapters,
                    &executor,
                    snapshot_metadata(&history, &witness),
                )
                .map_err(trace_wire)?,
            )
            .map_err(internal_wire)?;
            crate::pagination::prepare_snapshot(envelope).map_err(pagination_wire)
        })
        .await?;
        self.pages
            .borrow_mut()
            .page_prepared("query.trace", &fingerprint, limit, envelope)
            .map_err(pagination_wire)
    }
}

impl DaemonHandler {
    /// The operator reader-state store, read fresh on every call.
    ///
    /// Unlike [`Self::flow_lineage`] and [`Self::flow_membership`] this is not
    /// cached: the store holds at most a few hundred toggled runs (it folds
    /// itself at [`crate::reader_state::READER_STATE_COMPACT_THRESHOLD`]) and
    /// query volume never approaches the per-flow-start rate that caching
    /// those ledgers exists to absorb. A read failure — a missing file, a
    /// truncated line, an operator's hand edit gone wrong — degrades to
    /// "nothing is archived" rather than failing the query that asked; see
    /// [`ReaderState::read_advisory`].
    async fn reader_state_advisory(&self) -> ReaderState {
        let data_dir = self.context.read().await.paths.data_dir.clone();
        ReaderState::read_advisory(&reader_state_path(&data_dir))
    }
}

fn query_wire(error: crate::query::QueryError) -> WireError {
    match error {
        crate::query::QueryError::UnknownPool(_) => WireError::not_found(error.to_string()),
        crate::query::QueryError::InvalidPool(_)
        | crate::query::QueryError::InvalidTimestamp(_) => WireError::invalid(error.to_string()),
    }
}

fn observability_wire(error: ObservabilityError) -> WireError {
    match error {
        ObservabilityError::InvalidTimestamp(_) | ObservabilityError::InvalidPosition(_) => {
            WireError::invalid(error.to_string())
        }
        ObservabilityError::UnknownJob(_)
        | ObservabilityError::UnknownCampaign(_)
        | ObservabilityError::UnknownCampaignObservation { .. }
        | ObservabilityError::UnknownAttempt { .. } => WireError::not_found(error.to_string()),
        ObservabilityError::InvalidRunProjection(_) => internal_wire(error),
    }
}

fn pagination_wire(error: PaginationError) -> WireError {
    match error {
        PaginationError::InvalidLimit
        | PaginationError::InvalidCursor
        | PaginationError::CursorMismatch => WireError::invalid(error.to_string()),
        PaginationError::CursorExpired => WireError::not_found(error.to_string()),
        PaginationError::InvalidEnvelope | PaginationError::ItemTooLarge => internal_wire(error),
    }
}

fn trace_wire(error: TraceError) -> WireError {
    match error {
        TraceError::UnknownJob(_) | TraceError::UnknownAttempt { .. } => {
            WireError::not_found(error.to_string())
        }
        TraceError::Io { .. } => internal_wire(error),
    }
}

fn change_wire(error: ChangeError) -> WireError {
    match error {
        ChangeError::Invalid(_) => WireError::invalid(error.to_string()),
        ChangeError::Io { .. } | ChangeError::Json(_) => internal_wire(error),
    }
}

fn trace_lanes(
    details: &[RowDetailFact],
    live: &[LiveJobFact],
    history: &crate::history::LifecycleSnapshot,
) -> Vec<TraceLane> {
    let mut lanes = BTreeMap::<(String, u32, u64), TraceLane>::new();
    for detail in details {
        lanes.insert(
            (detail.task_uuid.clone(), detail.attempt, detail.lease_epoch),
            TraceLane {
                task_uuid: detail.task_uuid.clone(),
                task_ref: detail
                    .orchestration
                    .as_ref()
                    .and_then(Orchestration::task_ref),
                job_id: None,
                attempt: detail.attempt,
                lease_epoch: detail.lease_epoch,
                adapter: detail.adapter.clone(),
                session_ref: detail.session_ref.clone(),
                context_tokens: detail.context_tokens,
                context_window: detail.context_window.map(|window| window.tokens),
                running: false,
                remote: detail.executor.is_some(),
            },
        );
    }
    for record in &history.records {
        let (Some(attempt), Some(lease_epoch)) = (record.fields.attempt, record.fields.lease_epoch)
        else {
            continue;
        };
        let key = (record.fields.task_uuid.clone(), attempt, lease_epoch);
        let lane = lanes.entry(key).or_insert_with(|| TraceLane {
            task_uuid: record.fields.task_uuid.clone(),
            task_ref: record.fields.task_ref.clone(),
            job_id: record.fields.job_id.clone(),
            attempt,
            lease_epoch,
            adapter: record
                .fields
                .agent
                .clone()
                .unwrap_or_else(|| "unknown".to_owned()),
            session_ref: record.fields.session_ref.clone(),
            context_tokens: record.fields.context_tokens,
            context_window: record.fields.context_window,
            running: false,
            remote: record.fields.executor.is_some(),
        });
        if record.fields.job_id.is_some() {
            lane.job_id.clone_from(&record.fields.job_id);
        }
        if record.fields.task_ref.is_some() {
            lane.task_ref.clone_from(&record.fields.task_ref);
        }
        if record.fields.agent.is_some() {
            lane.adapter = record.fields.agent.clone().unwrap();
        }
        if record.fields.session_ref.is_some() {
            lane.session_ref.clone_from(&record.fields.session_ref);
        }
        if record.fields.context_tokens.is_some() {
            lane.context_tokens = record.fields.context_tokens;
        }
        if record.fields.context_window.is_some() {
            lane.context_window = record.fields.context_window;
        }
        lane.remote |= record.fields.executor.is_some();
    }
    for live in live {
        let key = (live.anchor.clone(), live.attempt, live.lease_epoch);
        if let Some(lane) = lanes.get_mut(&key) {
            lane.running = live.live_state == "running";
            lane.job_id = Some(live.job_id.clone());
        }
    }
    lanes.into_values().collect()
}

fn pool_headroom_facts(context: &mut Context) -> Result<Vec<PoolHeadroomFact>, WireError> {
    let now = Utc::now();
    let pools = context.config.pools.clone();
    let unleased_by_pool = context
        .jobs
        .values()
        .filter(|job| job.state == JobState::Queued && job.lease_id.is_none())
        .fold(HashMap::<String, usize>::new(), |mut counts, job| {
            for pool in &job.row.pools {
                *counts.entry(pool.clone()).or_default() += 1;
            }
            counts
        });
    pools
        .into_iter()
        .map(|(name, pool)| {
            let held = context
                .lease
                .engine()
                .held_in_pool(&name)
                .map_err(lease_wire)?;
            let queued = context
                .lease
                .engine()
                .queued_in_pool(&name)
                .map_err(lease_wire)?
                + unleased_by_pool.get(&name).copied().unwrap_or(0);
            let consumption = match pool.predicate {
                PoolPredicate::CoResidency(_) => None,
                PoolPredicate::WindowedConsumption(ref window) => {
                    let used = context
                        .lease
                        .engine_mut()
                        .budget_used_at(&name, now)
                        .map_err(lease_wire)?;
                    let reset_at = context
                        .lease
                        .engine_mut()
                        .window_reset_at(&name, now)
                        .map_err(lease_wire)?;
                    Some(WindowConsumptionFact {
                        used,
                        cap: window.consumption_cap,
                        reset_at,
                    })
                }
            };
            let meter = match (&pool.usage_meter, &pool.predicate) {
                (Some(meter), _) => read_usage_meter(
                    &context.paths.state_dir,
                    &name,
                    meter.poll_interval_sec.saturating_mul(2),
                    now,
                ),
                (None, PoolPredicate::WindowedConsumption(window))
                    if pool.resource() == crate::config::ResourceKind::Budget =>
                {
                    read_usage_meter(&context.paths.state_dir, &name, window.window_sec, now)
                }
                _ => None,
            };
            Ok(PoolHeadroomFact {
                pool: name,
                capacity: u64::from(pool.capacity),
                held: u64::try_from(held).unwrap_or(u64::MAX),
                queued,
                consumption,
                meter_utilization_pct: meter.as_ref().map(|meter| meter.utilization_pct),
                weekly_utilization_pct: meter.and_then(|meter| meter.weekly_utilization_pct),
            })
        })
        .collect()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageMeterObservation {
    pub(crate) pool: String,
    pub(crate) budget_class: crate::config::MeterBudgetClass,
    pub(crate) utilization_pct: f64,
    #[serde(default)]
    pub(crate) weekly_utilization_pct: Option<f64>,
    pub(crate) reset_at: String,
    pub(crate) observed_at: String,
}

pub(crate) fn usage_meter_event_path(state_dir: &Path, pool: &str) -> PathBuf {
    let digest = Sha256::digest(pool.as_bytes());
    state_dir.join("meters").join(format!("{digest:x}.json"))
}

pub(crate) fn read_usage_meter(
    state_dir: &Path,
    pool: &str,
    freshness_sec: u64,
    now: chrono::DateTime<Utc>,
) -> Option<UsageMeterObservation> {
    let path = usage_meter_event_path(state_dir, pool);
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_METER_EVENT_BYTES {
        return None;
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if !opened.file_type().is_file() || opened.len() > MAX_METER_EVENT_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_METER_EVENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_METER_EVENT_BYTES {
        return None;
    }
    let event: UsageMeterObservation = serde_json::from_slice(&bytes).ok()?;
    if event.pool != pool
        || event.budget_class != crate::config::MeterBudgetClass::Programmatic
        || !event.utilization_pct.is_finite()
        || !(0.0..=100.0).contains(&event.utilization_pct)
        || event
            .weekly_utilization_pct
            .is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value))
    {
        return None;
    }
    let observed_at = chrono::DateTime::parse_from_rfc3339(&event.observed_at)
        .ok()?
        .with_timezone(&Utc);
    let reset_at = chrono::DateTime::parse_from_rfc3339(&event.reset_at)
        .ok()?
        .with_timezone(&Utc);
    let freshness_sec = i64::try_from(freshness_sec).ok()?;
    if observed_at > now
        || now.signed_duration_since(observed_at) > chrono::Duration::seconds(freshness_sec)
        || reset_at <= now
        || reset_at < observed_at
    {
        return None;
    }
    Some(event)
}

/// Feed the built-in usage meter from exact per-attempt accounting.
///
/// A cumulative raw observation is never a meter input. When the declared
/// formula cannot be reduced exactly, no advisory meter event is written and
/// the returned typed diagnostic is logged; the durable admission debit
/// remains the conservative floor.
pub(crate) fn feed_scraped_usage(
    state_dir: &Path,
    pools: &BTreeMap<String, crate::config::PoolConfig>,
    leased_pools: &[String],
    usage: &crate::usage::UsageEvidence,
) -> Vec<String> {
    let amount = match usage.meter_amount() {
        Ok(Some(amount)) => amount,
        Ok(None) => return Vec::new(),
        Err(reason) => {
            return vec![format!(
                "usage-accounting-unavailable:{}",
                serde_json::to_value(reason)
                    .ok()
                    .and_then(|value| value.as_str().map(ToOwned::to_owned))
                    .unwrap_or_else(|| "unknown".to_owned())
            )];
        }
    };
    let observed_at = Utc::now();
    leased_pools
        .iter()
        .filter_map(|name| {
            let pool = pools.get(name)?;
            let PoolPredicate::WindowedConsumption(window) = &pool.predicate else {
                return None;
            };
            if pool.resource() != crate::config::ResourceKind::Budget || pool.usage_meter.is_some()
            {
                return None;
            }
            let reset_at = observed_at.checked_add_signed(chrono::Duration::seconds(
                i64::try_from(window.window_sec).ok()?,
            ))?;
            let event = UsageMeterObservation {
                pool: name.clone(),
                budget_class: crate::config::MeterBudgetClass::Programmatic,
                utilization_pct: ((amount as f64 / window.consumption_cap as f64) * 100.0)
                    .min(100.0),
                weekly_utilization_pct: None,
                reset_at: reset_at.to_rfc3339_opts(SecondsFormat::Millis, true),
                observed_at: observed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
            };
            write_usage_meter(state_dir, &event)
                .err()
                .map(|error| format!("pool {name:?}: {error}"))
        })
        .collect()
}

pub(crate) fn write_usage_meter(state_dir: &Path, event: &UsageMeterObservation) -> io::Result<()> {
    let directory = state_dir.join("meters");
    std::fs::create_dir_all(&directory)?;
    let metadata = std::fs::symlink_metadata(&directory)?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "meter directory is not a regular directory",
        ));
    }
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let path = usage_meter_event_path(state_dir, &event.pool);
    let temporary = directory.join(format!(".{}.tmp", Uuid::new_v4()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, event).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temporary, &path)?;
        File::open(&directory)?.sync_all()
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

pub(crate) fn overlay_live_states(
    jobs: &mut [JobProjection],
    live_states: &HashMap<String, String>,
) {
    for job in jobs {
        // A witness read happens after the live snapshot. If the job completed
        // in between, the newer terminal witness must win over stale live state.
        if job.witness_seq.is_none() {
            if let Some(state) = live_states.get(&job.anchor) {
                job.state.clone_from(state);
            }
        }
    }
}

pub(crate) fn query_row(row: &RowSeed, status: RowStatus) -> RowFact {
    RowFact {
        task_uuid: row.uuid.to_string(),
        description: row.description.clone(),
        argv: row.argv.clone(),
        brief_hash: row.brief_hash.clone(),
        orchestration: row.orchestration.clone(),
        status,
        priority: priority_name(row.priority).to_owned(),
        pools: Some(row.pools.clone()),
        executor: row.executor.clone(),
        source: Some(source_name(row.source).to_owned()),
        session_ref: row.session_ref.clone(),
        final_message: row.final_message.clone(),
        cwd: row
            .cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().into_owned()),
        workspace: row.workspace.clone(),
        resumed_from: row.resumed_from.clone(),
        attempt: row.attempt,
        model: row.model.clone(),
        gh_origin: row
            .gh_origin
            .as_ref()
            .and_then(crate::query::GhOriginProjection::from_origin),
        related_trigger: row.related_trigger.clone(),
    }
}

fn priority_name(priority: Priority) -> &'static str {
    match priority {
        Priority::Interrupt => "interrupt",
        Priority::High => "high",
        Priority::Medium => "medium",
        Priority::Low => "low",
    }
}

fn source_name(source: EnqueueSource) -> &'static str {
    match source {
        EnqueueSource::Manual => "manual",
        EnqueueSource::Orchestrator => "orchestrator",
        EnqueueSource::Calendar => "calendar",
        EnqueueSource::EventsDir => "events-dir",
        EnqueueSource::Gh => "gh",
        EnqueueSource::BuildEffect => "build-effect",
        EnqueueSource::PoolReachability => "pool-reachability",
    }
}
