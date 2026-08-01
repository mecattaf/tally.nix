use super::text::sanitize_line;
use super::*;

const SMOKE_RUNTIME_MAX_SEC: u64 = 5 * 60;
const CAPTURE_PROJECTION_TIMEOUT: Duration = Duration::from_secs(10);
const CAPTURE_PROJECTION_POLL: Duration = Duration::from_millis(100);

pub(super) async fn run_adapter(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    command: AdapterCommand,
) -> Result<()> {
    match command {
        AdapterCommand::Smoke(args) => {
            run_adapter_smoke(socket, config_path, rpc_timeout, args).await
        }
    }
}

async fn run_adapter_smoke(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: AdapterSmokeArgs,
) -> Result<()> {
    if args.name.trim().is_empty() || args.name.chars().any(char::is_control) {
        return Err(invalid(
            "adapter smoke name must not be empty or contain control characters",
        ));
    }
    if args.prompt.trim().is_empty() || args.prompt.contains('\0') {
        return Err(invalid(
            "adapter smoke prompt must not be empty or contain NUL bytes",
        ));
    }

    let config = load_client_config(config_path)?;
    let adapter = config.adapters.get(&args.name).ok_or_else(|| {
        invalid(format!(
            "unknown adapter {:?}; configured adapters: {}",
            args.name,
            configured_names(config.adapters.keys())
        ))
    })?;
    let pool = resolve_smoke_pool(&args.name, args.pool.as_deref(), &config.pools)?;
    let cwd = resolve_smoke_cwd(args.cwd)?;
    let required_captures = ["sessionRef", "finalMessage"]
        .into_iter()
        .filter(|name| adapter.scrape.contains_key(*name))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let label = format!("adapter-smoke:{}", args.name);
    let workload_argv = if args.name == "shell" {
        vec![
            std::env::current_exe()
                .context("cannot resolve tally executable for shell adapter smoke")?
                .display()
                .to_string(),
            "__adapter-smoke-shell".to_owned(),
        ]
    } else {
        vec![args.prompt]
    };
    let payload = EnqueuePayload {
        invocation: None,
        argv: Some(workload_argv),
        pools: Some(vec![pool.clone()]),
        executor: None,
        priority: Some(Priority::Medium),
        adapter: Some(args.name.clone()),
        cwd: Some(cwd.clone()),
        workspace: None,
        adapter_options: None,
        gate_manifest: None,
        brief: None,
        brief_path: None,
        resume_from: None,
        source: Some(EnqueueSource::Manual),
        dedup_key: None,
        submission: None,
        orchestration: None,
        parent: None,
        evidence: vec!["exit:0".to_owned()],
        drv: None,
        evidence_class: Some(json!({
            "kind": "adapter-smoke",
            "label": label.clone(),
            "adapter": args.name.clone(),
        })),
        manifest_hash: None,
        consumption_estimate: None,
        runtime_max_sec: Some(SMOKE_RUNTIME_MAX_SEC),
        no_enqueue: true,
        credentials: Default::default(),
        origin: None,
        caller_job_id: inherited_caller_job_id(),
        caller_job_token: inherited_caller_job_token(),
        gh_trigger_actor: None,
        gh_self_actor: None,
        gh_origin: None,
        task_uuid: None,
        related_trigger: None,
        wait: true,
    };

    let client = connect_rpc(socket, config_path).await?;
    let admitted = client
        .call("queue.enqueue", Some(serde_json::to_value(payload)?))
        .await?;
    let terminal = if admitted.get("verdict").and_then(Value::as_str).is_some() {
        admitted
    } else {
        let task_uuid = admitted
            .get("task_uuid")
            .and_then(Value::as_str)
            .filter(|task_uuid| !task_uuid.is_empty())
            .ok_or_else(|| invalid("queue.enqueue returned no task_uuid for adapter smoke"))?;
        await_job_with_rearm(client, socket, task_uuid, rpc_timeout).await?
    };

    let exit_code = waited_exit_code(&terminal);
    if exit_code != 0 {
        print_smoke_result(
            &args.name,
            &label,
            &pool,
            &cwd,
            &terminal,
            &required_captures,
            &BTreeMap::new(),
            "not-checked",
        )?;
        print_captured_stderr(&args.name, &terminal);
        let verdict = terminal
            .get("verdict")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(exit_failure(
            exit_code,
            format!(
                "adapter smoke {:?} finished with verdict {verdict}",
                args.name
            ),
        ));
    }

    let (captures, missing) =
        await_declared_captures(socket, config_path, &terminal, &required_captures).await?;
    let capture_status = if required_captures.is_empty() {
        "not-declared"
    } else if missing.is_empty() {
        "verified"
    } else {
        "missing"
    };
    print_smoke_result(
        &args.name,
        &label,
        &pool,
        &cwd,
        &terminal,
        &required_captures,
        &captures,
        capture_status,
    )?;
    if missing.is_empty() {
        Ok(())
    } else {
        Err(exit_failure(
            1,
            format!(
                "adapter smoke {:?} passed execution but did not project declared capture(s) {} within {} seconds",
                args.name,
                missing.join(", "),
                CAPTURE_PROJECTION_TIMEOUT.as_secs()
            ),
        ))
    }
}

fn resolve_smoke_cwd(cwd: Option<PathBuf>) -> Result<PathBuf> {
    let current =
        std::env::current_dir().context("cannot resolve adapter smoke working directory")?;
    Ok(match cwd {
        Some(path) if path.is_absolute() => path,
        Some(path) => current.join(path),
        None => current,
    })
}

fn resolve_smoke_pool(
    adapter: &str,
    requested: Option<&str>,
    pools: &BTreeMap<String, tally_core::config::PoolConfig>,
) -> Result<String> {
    if let Some(requested) = requested {
        if pools.contains_key(requested) {
            return Ok(requested.to_owned());
        }
        return Err(invalid(format!(
            "unknown pool {requested:?}; configured pools: {}",
            configured_names(pools.keys())
        )));
    }

    let candidates = match adapter {
        "shell" => vec!["build".to_owned(), "stock".to_owned(), "shell".to_owned()],
        "codex" => vec!["codex-window".to_owned(), "codex".to_owned()],
        "claude-code" => vec!["claude-window".to_owned(), "claude-code".to_owned()],
        "pi" => vec!["pi-window".to_owned(), "pi".to_owned()],
        other => vec![format!("{other}-window"), other.to_owned()],
    };
    if let Some(pool) = candidates.into_iter().find(|name| pools.contains_key(name)) {
        return Ok(pool);
    }
    Err(invalid(format!(
        "adapter {adapter:?} has no configured conventional pool; pass --pool NAME (configured pools: {})",
        configured_names(pools.keys())
    )))
}

fn configured_names<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let names = names.map(String::as_str).collect::<Vec<_>>();
    if names.is_empty() {
        "<none>".to_owned()
    } else {
        names.join(", ")
    }
}

async fn await_declared_captures(
    socket: &Path,
    config_path: Option<&Path>,
    terminal: &Value,
    required: &[String],
) -> Result<(BTreeMap<String, Value>, Vec<String>)> {
    if required.is_empty() {
        return Ok((BTreeMap::new(), Vec::new()));
    }
    let task_uuid = terminal
        .get("task_uuid")
        .or_else(|| terminal.get("taskUuid"))
        .and_then(Value::as_str)
        .filter(|task_uuid| !task_uuid.is_empty())
        .ok_or_else(|| invalid("adapter smoke terminal result has no task UUID"))?;
    let deadline = tokio::time::Instant::now() + CAPTURE_PROJECTION_TIMEOUT;
    let client = connect_rpc(socket, config_path).await?;
    loop {
        let result = client
            .call_with_deadline(
                "query.job",
                Some(json!({"id": task_uuid})),
                CAPTURE_PROJECTION_TIMEOUT,
            )
            .await?;
        let job = result
            .get("job")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("query.job returned no job object during adapter smoke"))?;
        let captures = required
            .iter()
            .filter_map(|name| {
                let value = job.get(name)?;
                let value = value.get("value").unwrap_or(value);
                value.is_string().then(|| (name.clone(), value.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let missing = required
            .iter()
            .filter(|name| !captures.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        if missing.is_empty() || tokio::time::Instant::now() >= deadline {
            return Ok((captures, missing));
        }
        tokio::time::sleep(CAPTURE_PROJECTION_POLL).await;
    }
}

#[allow(clippy::too_many_arguments)]
fn print_smoke_result(
    adapter: &str,
    label: &str,
    pool: &str,
    cwd: &Path,
    terminal: &Value,
    declared_captures: &[String],
    captures: &BTreeMap<String, Value>,
    capture_status: &str,
) -> Result<()> {
    let field = |snake: &str, camel: &str| {
        terminal
            .get(snake)
            .or_else(|| terminal.get(camel))
            .cloned()
            .unwrap_or(Value::Null)
    };
    println!(
        "{}",
        serde_json::to_string(&json!({
            "schemaVersion": 1,
            "diagnostic": "adapter-smoke",
            "label": label,
            "adapter": adapter,
            "pool": pool,
            "cwd": cwd,
            "taskUuid": field("task_uuid", "taskUuid"),
            "attempt": field("attempt", "attempt"),
            "leaseEpoch": field("lease_epoch", "leaseEpoch"),
            "verdict": field("verdict", "verdict"),
            "exitCode": field("exit_code", "exitCode"),
            "witnessSeq": field("witness_seq", "witnessSeq"),
            "declaredCaptures": declared_captures,
            "captures": captures,
            "captureStatus": capture_status,
        }))?
    );
    Ok(())
}

fn print_captured_stderr(adapter: &str, terminal: &Value) {
    let excerpt = terminal
        .get("stderr_excerpt")
        .or_else(|| terminal.get("stderrExcerpt"))
        .and_then(Value::as_str)
        .filter(|excerpt| !excerpt.is_empty());
    match excerpt {
        Some(excerpt) => {
            eprintln!("adapter smoke {adapter:?} captured stderr:");
            // The excerpt is whatever the adapter wrote; printing it verbatim
            // hands control of the operator's terminal to a failing job.
            for line in excerpt.lines() {
                eprintln!("{}", sanitize_line(line));
            }
        }
        None => eprintln!("adapter smoke {adapter:?} captured stderr was empty"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tally_core::config::PoolConfig;

    fn pools(names: &[&str]) -> BTreeMap<String, PoolConfig> {
        names
            .iter()
            .map(|name| ((*name).to_owned(), PoolConfig::default()))
            .collect()
    }

    #[test]
    fn conventional_pool_resolution_is_deterministic() {
        let configured = pools(&["build", "codex-window", "local-ai-review"]);
        assert_eq!(
            resolve_smoke_pool("shell", None, &configured).unwrap(),
            "build"
        );
        assert_eq!(
            resolve_smoke_pool("codex", None, &configured).unwrap(),
            "codex-window"
        );
        assert_eq!(
            resolve_smoke_pool("codex", Some("local-ai-review"), &configured).unwrap(),
            "local-ai-review"
        );
        assert!(resolve_smoke_pool("pi", None, &configured).is_err());
    }

    #[test]
    fn stock_is_a_conventional_shell_lane() {
        assert_eq!(
            resolve_smoke_pool("shell", None, &pools(&["stock"])).unwrap(),
            "stock"
        );
    }

    #[test]
    fn unrelated_or_absent_pool_requires_an_override() {
        assert!(resolve_smoke_pool("shell", None, &pools(&["worker"])).is_err());
        assert!(resolve_smoke_pool("shell", None, &pools(&[])).is_err());
    }
}
