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
            println!("{}", serde_json::to_string(&result)?);
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
        } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.jobs",
                Some(json!({
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
                })),
            )
            .await
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
        QueryCommand::Log {
            task,
            attempt,
            session,
            event,
            source,
            since,
            until,
            limit,
            cursor,
        } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.log",
                Some(json!({
                    "task": task,
                    "attempt": attempt,
                    "session": session,
                    "event": event,
                    "source": source,
                    "since": since,
                    "until": until,
                    "limit": limit,
                    "cursor": cursor,
                })),
            )
            .await
        }
        QueryCommand::Proof { task, attempt } => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "query.proof",
                Some(json!({"task": task, "attempt": attempt})),
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
                println!(
                    "{}",
                    result
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("daemon returned non-text render output"))?
                );
            } else {
                println!("{}", serde_json::to_string(&result)?);
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
    println!("{}", serde_json::to_string(&result)?);
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
            println!("{}", serde_json::to_string(&result)?);
            return Err(invalid(format!(
                "query watch cursor expired; earliest available is {}",
                result["earliestAvailableCursor"]
            )));
        }
        let items = result["items"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("daemon returned an invalid watch response"))?;
        for item in items {
            println!("{}", serde_json::to_string(item)?);
        }
        after = result["nextCursor"].as_str().map(ToOwned::to_owned);
        if once {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}
