use super::out::{errln, outln};
use super::text::{compact_text, sanitize_line};
use super::*;

pub(super) async fn run_queue(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    command: QueueCommand,
) -> Result<()> {
    match command {
        QueueCommand::Enqueue(_) => unreachable!("enqueue is routed before run_queue"),
        QueueCommand::Cancel { job, force } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "queue.cancel",
                Some(json!({"task_uuid": job, "force": force})),
            )
            .await
        }
        QueueCommand::Pause { pool, all } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "queue.pause",
                Some(json!({"pool": pool, "all": all})),
            )
            .await
        }
        QueueCommand::Resume { pool, all } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "queue.resume",
                Some(json!({"pool": pool, "all": all})),
            )
            .await
        }
        QueueCommand::Continue { job, wait, argv } => {
            let payload = EnqueuePayload {
                invocation: None,
                argv: Some(argv),
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
                resume_from: Some(job),
                source: None,
                dedup_key: None,
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
                caller_job_id: inherited_caller_job_id(),
                caller_job_token: inherited_caller_job_token(),
                gh_trigger_actor: None,
                gh_self_actor: None,
                gh_origin: None,
                task_uuid: None,
                related_trigger: None,
                wait,
            };
            submit_payload(
                socket,
                config_path,
                rpc_timeout,
                "queue.continue",
                payload,
                wait,
            )
            .await
        }
        QueueCommand::Retry { job } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "queue.retry",
                Some(json!({"task_uuid": job})),
            )
            .await
        }
        QueueCommand::Drain => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "queue.drain",
                Some(json!({})),
            )
            .await
        }
        QueueCommand::AwaitJob { job } => {
            let client = connect_rpc(socket, config_path).await?;
            let result = await_job_with_rearm(client, socket, &job, rpc_timeout).await?;
            outln!("{}", serde_json::to_string(&result)?);
            Ok(())
        }
        QueueCommand::AwaitBarrier { barrier } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "queue.await_barrier",
                Some(json!({"barrier": barrier})),
            )
            .await
        }
    }
}

pub(super) async fn run_lease(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    command: LeaseCommand,
) -> Result<()> {
    match command {
        LeaseCommand::Acquire { mut pools } => {
            tally_core::poolset::canonicalize(&mut pools)
                .map_err(|error| invalid(error.to_string()))?;
            let pool = match pools.as_slice() {
                [pool] => Value::String(pool.clone()),
                pools => serde_json::to_value(pools)?,
            };
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "lease.acquire",
                Some(json!({"pool": pool})),
            )
            .await
        }
        LeaseCommand::Release { lease } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "lease.release",
                Some(json!({"lease": lease})),
            )
            .await
        }
        LeaseCommand::Status { lease } => {
            let params = if let Some(lease) = lease {
                json!({"lease": lease})
            } else {
                let job_id = std::env::var("TALLY_JOB_ID").map_err(|_| {
                    invalid("lease status requires a lease argument or TALLY_JOB_ID")
                })?;
                json!({"jobId": job_id})
            };
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "lease.status",
                Some(params),
            )
            .await
        }
    }
}

pub(super) async fn run_query(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    command: QueryCommand,
) -> Result<()> {
    match command {
        QueryCommand::Jobs {
            state,
            verdict,
            pool,
            executor,
            adapter,
            source,
            origin,
            parent,
            flow_run,
            session,
            since,
            until,
            limit,
            cursor,
            json,
        } => {
            let params = json!({
                "liveState": state,
                "terminalVerdict": verdict,
                "pool": pool,
                "executor": executor,
                "adapter": adapter,
                "source": source,
                "origin": origin,
                "parent": parent,
                "flowRun": flow_run,
                "session": session,
                "since": since,
                "until": until,
                "limit": limit,
                "cursor": cursor,
            });
            let client = connect_rpc(socket, config_path).await?;
            // A caller that supplied its own cursor, or asked for `--json`,
            // owns pagination. Everyone else gets the whole window.
            if json || cursor.is_some() {
                let result = call_page(&client, rpc_timeout, "query.jobs", params).await?;
                report_page_completeness(&result)?;
                outln!("{}", serde_json::to_string(&result)?);
                return Ok(());
            }
            let window = collect_window(&client, rpc_timeout, "query.jobs", &params).await?;
            window.report()?;
            outln!("{}", serde_json::to_string(&window.envelope)?);
            Ok(())
        }
        QueryCommand::Job { id } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.job",
                Some(json!({"id": id})),
            )
            .await
        }
        QueryCommand::Run { id, json, status } => {
            let client = connect_rpc(socket, config_path).await?;
            let result = client
                .call_with_deadline("query.run", Some(json!({"id": id})), rpc_timeout)
                .await?;
            if json {
                outln!("{}", serde_json::to_string(&result)?);
                Ok(())
            } else {
                print_run_human(&result, status.map(RunTaskFilter::as_str))
            }
        }
        QueryCommand::Status { pool } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.status",
                Some(json!({"pool": pool})),
            )
            .await
        }
        QueryCommand::Storage => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.storage",
                Some(json!({})),
            )
            .await
        }
        QueryCommand::Log {
            task,
            flow_run,
            attempt,
            session,
            event,
            source,
            since,
            until,
            limit,
            cursor,
            after,
            json,
            provenance,
        } => {
            let params = json!({
                "task": task,
                "flowRun": flow_run,
                "attempt": attempt,
                "session": session,
                "event": event,
                "source": source,
                "since": since,
                "until": until,
                "limit": limit,
                "cursor": cursor,
                "after": after,
                "provenance": provenance,
            });
            let client = connect_rpc(socket, config_path).await?;
            // `--json` and an explicit `--cursor` keep single-page semantics:
            // the caller owns the cursor. The human view owns it instead, so
            // it walks to the end of the window before printing anything.
            if json || cursor.is_some() {
                let result = call_page(&client, rpc_timeout, "query.log", params).await?;
                report_page_completeness(&result)?;
                outln!("{}", serde_json::to_string(&result)?);
                return Ok(());
            }
            let window = collect_window(&client, rpc_timeout, "query.log", &params).await?;
            print_lifecycle_human(&window.envelope, provenance)?;
            window.report()?;
            Ok(())
        }
        QueryCommand::Proof {
            task,
            flow_run,
            attempt,
        } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.proof",
                Some(json!({
                    "task": task,
                    "flowRun": flow_run,
                    "attempt": attempt,
                })),
            )
            .await
        }
        QueryCommand::Trace {
            task,
            attempt,
            limit,
            cursor,
        } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.trace",
                Some(json!({
                    "task": task,
                    "attempt": attempt,
                    "limit": limit,
                    "cursor": cursor,
                })),
            )
            .await
        }
        QueryCommand::Producers { name, kind } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.producers",
                Some(json!({"name": name, "kind": kind})),
            )
            .await
        }
        QueryCommand::Watch { after, once } => {
            run_query_watch(socket, config_path, after, once).await
        }
        QueryCommand::Render { format } => {
            let client = connect_rpc(socket, config_path).await?;
            let result = client
                .call("query.render", Some(json!({"format": format.clone()})))
                .await?;
            if format == "text" {
                outln!(
                    "{}",
                    result
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("daemon returned non-text render output"))?
                );
            } else {
                outln!("{}", serde_json::to_string(&result)?);
            }
            Ok(())
        }
        QueryCommand::Standup { since } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.standup",
                Some(json!({"since": since})),
            )
            .await
        }
        QueryCommand::Pools => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.pools",
                Some(json!({})),
            )
            .await
        }
    }
}

/// One filtered window, assembled by following page cursors within a single
/// invocation. `envelope` is the last page's envelope with every page's items
/// merged back in, so its `snapshot` and `position` still describe the one
/// frozen projection all the pages came from.
pub(super) struct PagedWindow {
    envelope: Value,
    pages: usize,
    elided: usize,
    restarted: bool,
}

impl PagedWindow {
    /// Say, on stderr, everything that stops this from being the whole window.
    /// Silence here means the reader is looking at all of it — that is the
    /// entire point of the line.
    fn report(&self) -> Result<()> {
        if self.restarted {
            errln!(
                "notice: the page cursor expired mid-window; the query was restarted once and \
                 this window was re-read from the beginning"
            );
        }
        if self.elided > 0 {
            errln!(
                "notice: {} item(s) exceeded the bounded response size; their largest fields were \
                 elided and marked with an `elided` object on the item",
                self.elided
            );
        }
        report_position_gap(&self.envelope)?;
        report_empty_flow_run(&self.envelope)?;
        if self.pages > 1 {
            errln!(
                "notice: this window was assembled from {} pages; it is complete as of snapshot {}",
                self.pages,
                compact_text(
                    self.envelope["snapshot"]["createdAt"]
                        .as_str()
                        .unwrap_or("unknown")
                )
            );
        }
        Ok(())
    }
}

/// Report what a single page cannot show. `--json` and an explicit `--cursor`
/// hand pagination to the caller, but the caller still has to be told when the
/// page in front of them is not the whole window.
fn report_page_completeness(envelope: &Value) -> Result<()> {
    if envelope["truncated"].as_bool() == Some(true) || envelope["nextCursor"].is_string() {
        errln!(
            "notice: this response is one page of a larger window; continue with --cursor {}",
            compact_text(envelope["nextCursor"].as_str().unwrap_or("<missing>"))
        );
    }
    if envelope["elidedItems"].as_u64().unwrap_or(0) > 0 {
        errln!(
            "notice: {} item(s) exceeded the bounded response size; their largest fields were \
             elided and marked with an `elided` object on the item",
            envelope["elidedItems"].as_u64().unwrap_or(0)
        );
    }
    report_position_gap(envelope)?;
    report_empty_flow_run(envelope)
}

/// A `--flow-run` window that resolved to no member tasks is not evidence
/// about the run. Run membership is recomputed per call from durable rows and
/// witness records, and an admission that wrote no row — `attached`, and
/// full-mode `reused` and `terminal` — leaves the run holding a task UUID that
/// is not a member of it. Reading that empty window as "quiet" is #247.
fn report_empty_flow_run(envelope: &Value) -> Result<()> {
    if envelope["flowRunTasks"].as_u64() == Some(0) {
        errln!(
            "notice: this flow run resolves to NO member tasks, so an empty window here says \
             nothing about whether it is running; a node admitted as attached/reused/terminal \
             writes no row and never joins the run that submitted it. Corroborate with \
             `tally query run <id>`, the runner unit, or the node's own task UUID."
        );
    }
    Ok(())
}

fn report_position_gap(envelope: &Value) -> Result<()> {
    if let Some(gap) = envelope.get("positionGap").filter(|gap| gap.is_object()) {
        errln!(
            "notice: --after {} predates retained lifecycle history (earliest available is {}); \
             events before that boundary are gone and this window is not a complete continuation",
            compact_text(gap["requested"].as_str().unwrap_or("<unknown>")),
            compact_text(gap["earliestAvailable"].as_str().unwrap_or("<unknown>"))
        );
    }
    Ok(())
}

async fn call_page(
    client: &RpcClient,
    rpc_timeout: Duration,
    method: &str,
    params: Value,
) -> Result<Value> {
    client
        .call_with_deadline(method, Some(params), rpc_timeout)
        .await
        .map_err(|error| annotate_page_error(method, error))
}

/// Turn the daemon's bounded-response errors into something a reader can act
/// on. The oversized-item failure in particular used to surface as an opaque
/// internal error with no hint that the query itself was still answerable.
fn annotate_page_error(method: &str, error: WireIoError) -> anyhow::Error {
    if let WireIoError::Rpc(WireErrorCode::Internal, message, _) = &error {
        if message.contains("exceeds the bounded response size") {
            return anyhow::anyhow!(
                "{method} could not render one item within the bounded response size even after \
                 eliding its largest text fields; narrow the query (for example with --task or \
                 --limit 1) to see the rest"
            );
        }
    }
    error.into()
}

fn is_cursor_expired(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<WireIoError>(),
        Some(WireIoError::Rpc(WireErrorCode::NotFound, message, _))
            if message.contains("cursor expired")
    )
}

/// Follow `nextCursor` to the end of the filtered window inside one
/// invocation. A page cursor is an ephemeral snapshot offset, so the walk can
/// lose its snapshot mid-window; that is recoverable exactly once, by starting
/// the window over, and it is never silent.
async fn collect_window(
    client: &RpcClient,
    rpc_timeout: Duration,
    method: &str,
    params: &Value,
) -> Result<PagedWindow> {
    match collect_window_once(client, rpc_timeout, method, params).await {
        Ok(mut window) => {
            window.restarted = false;
            Ok(window)
        }
        Err(error) if is_cursor_expired(&error) => {
            let mut window = collect_window_once(client, rpc_timeout, method, params)
                .await
                .map_err(|second| {
                    second.context(
                        "the page cursor expired twice while assembling this window; the daemon \
                         is evicting snapshots faster than this query can be read",
                    )
                })?;
            window.restarted = true;
            Ok(window)
        }
        Err(error) => Err(error),
    }
}

async fn fetch_page(
    client: &RpcClient,
    rpc_timeout: Duration,
    method: &str,
    params: &Value,
    cursor: Option<String>,
) -> Result<Value> {
    let mut call_params = params.clone();
    call_params["cursor"] = cursor.map_or(Value::Null, Value::String);
    call_page(client, rpc_timeout, method, call_params).await
}

async fn collect_window_once(
    client: &RpcClient,
    rpc_timeout: Duration,
    method: &str,
    params: &Value,
) -> Result<PagedWindow> {
    let mut items = Vec::new();
    let mut pages = 0_usize;
    let mut elided = 0_usize;
    let mut envelope = fetch_page(client, rpc_timeout, method, params, None).await?;
    loop {
        pages += 1;
        elided += usize::try_from(envelope["elidedItems"].as_u64().unwrap_or(0)).unwrap_or(0);
        match envelope["items"].take() {
            Value::Array(page_items) => items.extend(page_items),
            _ => {
                return Err(anyhow::anyhow!(
                    "daemon returned an invalid {method} response with no items array"
                ))
            }
        }
        let Some(cursor) = envelope["nextCursor"].as_str().map(ToOwned::to_owned) else {
            break;
        };
        envelope = fetch_page(client, rpc_timeout, method, params, Some(cursor)).await?;
    }
    envelope["items"] = Value::Array(items);
    envelope["nextCursor"] = Value::Null;
    envelope["truncated"] = Value::Bool(false);
    envelope["elidedItems"] = Value::from(elided);
    Ok(PagedWindow {
        envelope,
        pages,
        elided,
        restarted: false,
    })
}

fn print_lifecycle_human(envelope: &Value, provenance: bool) -> Result<()> {
    let items = envelope
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid lifecycle log response"))?;
    if items.is_empty() {
        outln!("No lifecycle transitions.");
    }
    for item in items {
        let timestamp = item["timestamp"].as_str().unwrap_or("unknown-time");
        let identity = item["taskRef"]
            .as_str()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| short_identity(item["taskUuid"].as_str().unwrap_or("unknown")));
        let label = item["nodeLabel"].as_str().unwrap_or("-");
        let state = item["terminalVerdict"]
            .as_str()
            .unwrap_or_else(|| human_event(item["event"].as_str().unwrap_or("unknown")));
        let mut detail = Vec::new();
        if matches!(item["event"].as_str(), Some("started" | "dispatched")) {
            if let Some(adapter) = item["adapter"].as_str() {
                detail.push(format!("adapter={}", compact_text(adapter)));
            }
            if let Some(pools) = item["pool"].as_array() {
                let pools = pools
                    .iter()
                    .filter_map(Value::as_str)
                    .map(compact_text)
                    .collect::<Vec<_>>();
                if !pools.is_empty() {
                    detail.push(format!("pool={}", pools.join(",")));
                }
            }
        }
        if let Some(seconds) = item["wallClockSeconds"].as_f64() {
            detail.push(format!("elapsed={}", human_seconds_f64(seconds)));
        }
        if let Some(exit_code) = item["exitCode"].as_i64() {
            detail.push(format!("exit={exit_code}"));
        }
        if let Some(stderr) = item["stderrTail"].as_str() {
            detail.push(format!(
                "stderr={}",
                serde_json::to_string(&compact_text(stderr))?
            ));
        }
        if let Some(attempt) = item["attempt"].as_u64() {
            detail.push(format!("attempt={attempt}"));
        }
        if provenance {
            let origin = compact_text(item["origin"].as_str().unwrap_or("unknown"));
            let source = compact_text(item["provenance"].as_str().unwrap_or("unknown"));
            detail.push(format!("provenance={origin}:{source}"));
        }
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!("  {}", detail.join(" "))
        };
        outln!(
            "{}  {:<14}  {:<24}  {}{}",
            compact_text(timestamp),
            compact_text(&identity),
            compact_text(label),
            compact_text(state),
            suffix
        );
    }
    // A non-null cursor here would mean the window was still short: the human
    // path follows cursors to the end, so reaching this line with one left is
    // a defect, not a hint to hand the reader.
    if let Some(cursor) = envelope["nextCursor"].as_str() {
        errln!(
            "notice: this listing is INCOMPLETE; more transitions remain after --cursor {}",
            compact_text(cursor)
        );
    }
    if let Some(position) = envelope["position"].as_str() {
        errln!("position: {}", compact_text(position));
    }
    Ok(())
}

fn print_run_human(run: &Value, status_filter: Option<&str>) -> Result<()> {
    let flow_run_id = run["flowRunId"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid run response"))?;
    let state = run["state"].as_str().unwrap_or("unknown");
    let flow_name = run["flowName"].as_str().unwrap_or("flow");
    let campaign = run["campaign"].as_str();
    outln!(
        "{}{}  {}  {}",
        compact_text(flow_name),
        campaign.map_or_else(String::new, |value| format!(" {}", compact_text(value))),
        compact_text(flow_run_id),
        compact_text(state)
    );

    let counts = &run["counts"];
    outln!(
        "Tasks: {} done, {} running, {} blocked, {} pending",
        counts["done"].as_u64().unwrap_or(0),
        counts["running"].as_u64().unwrap_or(0),
        counts["blocked"].as_u64().unwrap_or(0),
        counts["pending"].as_u64().unwrap_or(0)
    );

    // Above the board, never inside it: a sub-issue closed with no merged
    // proof is a contradiction between what the forge shows a reader and what
    // the campaign can prove, and a reader who misses it debugs the wrong
    // surface.
    let anomalies = run["anomalies"].as_array().map_or(&[][..], Vec::as_slice);
    if !anomalies.is_empty() {
        outln!();
        outln!(
            "!! ANOMALIES: {} closed sub-issue(s) hold no merged proof; those tasks are NOT done",
            anomalies.len()
        );
        for anomaly in anomalies {
            outln!(
                "  !! {}  {}  {}",
                compact_text(anomaly["taskRef"].as_str().unwrap_or("-")),
                compact_text(anomaly["url"].as_str().unwrap_or("-")),
                compact_text(anomaly["detail"].as_str().unwrap_or("-"))
            );
        }
    }
    let tasks = run["tasks"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid run task table"))?;
    // The counts above stay whole-run; the filter narrows the board only.
    let shown = tasks
        .iter()
        .filter(|task| status_filter.is_none_or(|status| task["status"] == status))
        .collect::<Vec<_>>();
    if tasks.is_empty() {
        outln!("No reconciled task table is available for this run.");
    } else if shown.is_empty() {
        outln!(
            "No task is {}.",
            status_filter.unwrap_or("in the requested state")
        );
    } else {
        outln!();
        outln!(
            "{:<9}  {:<18}  {:<24}  TITLE",
            "STATUS",
            "TASK",
            "CURRENT / BLOCKED BY"
        );
        for task in shown {
            let task_ref = task["taskRef"].as_str().unwrap_or("-");
            let context = task["failureStage"]
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| task["currentNode"].as_str().map(ToOwned::to_owned))
                .or_else(|| {
                    let blocked = task["blockedBy"]
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>();
                    (!blocked.is_empty()).then(|| blocked.join(","))
                })
                .or_else(|| task["pullRequest"].as_str().map(ToOwned::to_owned))
                .unwrap_or_else(|| "-".to_owned());
            outln!(
                "{:<9}  {:<18}  {:<24}  {}",
                compact_text(task["status"].as_str().unwrap_or("unknown")),
                compact_text(task_ref),
                compact_text(&context),
                compact_text(task["title"].as_str().unwrap_or("-"))
            );
        }
    }

    let current_nodes = run["currentNodes"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid current-node table"))?;
    if !current_nodes.is_empty() {
        outln!();
        outln!("Current nodes");
        for node in current_nodes {
            let identity = node["taskRef"]
                .as_str()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| short_identity(node["taskUuid"].as_str().unwrap_or("unknown")));
            // A node with no start event has not begun; "unknown" read as a
            // missing measurement rather than the absence of one.
            let elapsed = node["elapsedSeconds"]
                .as_u64()
                .map(human_seconds)
                .unwrap_or_else(|| "not-started".to_owned());
            let budget = node["budgetRemainingSeconds"]
                .as_i64()
                .map(human_seconds_signed)
                .unwrap_or_else(|| "unbounded".to_owned());
            outln!(
                "  {:<18}  {:<24}  {:<10}  elapsed={} budget={}",
                compact_text(&identity),
                compact_text(node["label"].as_str().unwrap_or("-")),
                compact_text(node["state"].as_str().unwrap_or("unknown")),
                elapsed,
                budget
            );
        }
    }

    let failures = run["failures"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid failure table"))?;
    if !failures.is_empty() {
        outln!();
        outln!("Failures");
        for failure in failures {
            let identity = failure["taskRef"]
                .as_str()
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| {
                    short_identity(failure["taskUuid"].as_str().unwrap_or("unknown"))
                });
            outln!(
                "  {}  {}  {}",
                compact_text(&identity),
                compact_text(failure["stage"].as_str().unwrap_or("unknown-stage")),
                compact_text(failure["verdict"].as_str().unwrap_or("failed"))
            );
            // Always emit the line. Doctrine sends operators and agents to the
            // capture first, and a silently omitted pointer cannot be told
            // apart from a capture that exists but failed to resolve.
            match failure["capturePath"].as_str() {
                Some(path) => outln!("    capture: {}", compact_text(path)),
                None => outln!("    capture: <not retained>"),
            }
            if let Some(stderr) = failure["stderrTail"].as_str() {
                let truncated = failure["stderrTruncated"].as_bool().unwrap_or(false);
                outln!(
                    "    stderr tail{}:",
                    if truncated { " (truncated)" } else { "" }
                );
                // Indentation is the structure of a stack trace or a diff, so
                // the tail keeps it; only terminal control is stripped.
                for line in stderr.lines() {
                    outln!("      {}", sanitize_line(line));
                }
            }
        }
    }
    Ok(())
}

fn human_event(event: &str) -> &str {
    match event {
        "enqueued" => "queued",
        "heartbeat" => "running",
        "completed" => "pass",
        "failed" => "failed",
        "evidence_pass" => "evidence-pass",
        "evidence_fail" => "evidence-fail",
        "witness_emitted" => "terminal",
        value => value,
    }
}

fn short_identity(value: &str) -> String {
    value.chars().take(8).collect()
}

fn human_seconds(seconds: u64) -> String {
    if seconds >= 3_600 {
        format!("{}h{:02}m", seconds / 3_600, seconds % 3_600 / 60)
    } else if seconds >= 60 {
        format!("{}m{:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{seconds}s")
    }
}

fn human_seconds_signed(seconds: i64) -> String {
    if seconds < 0 {
        format!("-{}", human_seconds(seconds.unsigned_abs()))
    } else {
        human_seconds(seconds.unsigned_abs())
    }
}

fn human_seconds_f64(seconds: f64) -> String {
    if seconds < 10.0 {
        format!("{seconds:.1}s")
    } else {
        human_seconds(seconds.round().max(0.0) as u64)
    }
}

pub(super) async fn print_rpc(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    method: &str,
    params: Option<Value>,
) -> Result<()> {
    let client = connect_rpc(socket, config_path).await?;
    let result = if method == "query.watch" {
        client.call(method, params).await?
    } else {
        client
            .call_with_deadline(method, params, rpc_timeout)
            .await?
    };
    outln!("{}", serde_json::to_string(&result)?);
    Ok(())
}

pub(super) async fn connect_rpc(socket: &Path, config_path: Option<&Path>) -> Result<RpcClient> {
    let max_frame_bytes = client_max_frame_bytes(config_path)?;
    RpcClient::connect_with_max_frame_bytes(socket, max_frame_bytes)
        .await
        .map_err(Into::into)
}

pub(super) fn client_max_frame_bytes(config_path: Option<&Path>) -> Result<u64> {
    resolve_max_frame_bytes(config_path).map_err(Into::into)
}

pub(super) fn load_client_config(config_path: Option<&Path>) -> Result<Config> {
    let path = config_path.map_or_else(default_config_path, |path| Ok(path.to_owned()))?;
    Config::from_path(&path).map_err(Into::into)
}

pub(super) async fn run_query_watch(
    socket: &Path,
    config_path: Option<&Path>,
    mut after: Option<String>,
    once: bool,
) -> Result<()> {
    let client = connect_rpc(socket, config_path).await?;
    loop {
        let result = client
            .call(
                "query.watch",
                Some(json!({"after": after.clone(), "limit": 100})),
            )
            .await?;
        if result["status"] == "cursor-expired" {
            outln!("{}", serde_json::to_string(&result)?);
            return Err(invalid(format!(
                "query watch cursor expired; earliest available is {}",
                result["earliestAvailableCursor"]
            )));
        }
        let items = result["items"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid watch response"))?;
        for item in items {
            outln!("{}", serde_json::to_string(item)?);
        }
        after = result["nextCursor"].as_str().map(ToOwned::to_owned);
        if once {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
