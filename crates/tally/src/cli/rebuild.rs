use super::*;

const LIVE_VERIFY_MAX_WAIT: Duration = Duration::from_secs(1);

enum LiveVerification {
    Equal,
    Unavailable(String),
}

/// Replay one run without depending on the daemon, then compare it with the
/// live derived projection when that projection answers promptly. A stalled or
/// absent daemon is the reason this verb exists, so inability to perform the
/// comparison is rendered as a caveat rather than making the rebuilt view
/// unavailable too.
pub(super) async fn run_rebuild(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    args: RebuildArgs,
) -> Result<()> {
    let paths = DaemonPaths {
        socket: socket.to_owned(),
        state_dir: args.state_dir.map_or_else(default_state_dir, Ok)?,
        data_dir: args.data_dir.map_or_else(default_data_dir, Ok)?,
    };
    let executor = rebuild_executor(config_path, &paths.state_dir)?;
    let rebuilt = rebuild_run_view(&paths, &args.id, &executor, Utc::now())
        .await
        .map_err(|error| match error {
            DurableViewError::Projection(ObservabilityError::UnknownJob(id)) => {
                exit_failure(4, format!("run {id} is not in canonical state"))
            }
            error => exit_failure(1, error.to_string()),
        })?;

    let verification = verify_live_if_available(
        socket,
        config_path,
        rpc_timeout.min(LIVE_VERIFY_MAX_WAIT),
        &args.id,
        &rebuilt.view,
    )
    .await?;
    let mut caveats = rebuilt.caveats;
    let verification_value = match verification {
        LiveVerification::Equal => json!({"status": "equal"}),
        LiveVerification::Unavailable(reason) => {
            caveats.push(format!(
                "live derived state was unavailable for comparison: {reason}"
            ));
            json!({"status": "unavailable", "reason": reason})
        }
    };

    let mut value = serde_json::to_value(&rebuilt.view)?;
    if let Some(object) = value.as_object_mut() {
        object.insert("view".to_owned(), json!("rebuild"));
        object.insert("live".to_owned(), json!(false));
        object.insert("unitLiveness".to_owned(), json!(true));
        object.insert("liveVerification".to_owned(), verification_value.clone());
        object.insert("caveats".to_owned(), json!(caveats));
    }
    if args.json {
        outln!("{}", serde_json::to_string(&value)?);
        return Ok(());
    }
    for caveat in value["caveats"].as_array().map_or(&[][..], Vec::as_slice) {
        if let Some(caveat) = caveat.as_str() {
            outln!("! {caveat}");
        }
    }
    if verification_value["status"] == "equal" {
        outln!("= live derived state matches the canonical replay");
    }
    print_run_human(&value, None)
}

fn rebuild_executor(config_path: Option<&Path>, state_dir: &Path) -> Result<Executor> {
    let program = std::env::current_exe().context("cannot resolve tally executable")?;
    let mut executor = Executor::new(state_dir, program);
    let config = match config_path {
        Some(path) => Some(Config::from_path(path)?),
        None => default_config_path()
            .ok()
            .filter(|path| path.exists())
            .map(|path| Config::from_path(&path))
            .transpose()?,
    };
    if let Some(config) = config {
        executor = executor.with_remote_executors(config.executors);
    }
    Ok(executor)
}

async fn verify_live_if_available(
    socket: &Path,
    config_path: Option<&Path>,
    deadline: Duration,
    flow_run: &str,
    rebuilt: &RunView,
) -> Result<LiveVerification> {
    let client = match tokio::time::timeout(deadline, connect_rpc(socket, config_path)).await {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => return Ok(LiveVerification::Unavailable(error.to_string())),
        Err(_) => {
            return Ok(LiveVerification::Unavailable(format!(
                "connection exceeded {:.3} s",
                deadline.as_secs_f64()
            )))
        }
    };
    let result = match tokio::time::timeout(
        deadline,
        client.call_with_deadline("query.run", Some(json!({"id": flow_run})), deadline),
    )
    .await
    {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => return Ok(LiveVerification::Unavailable(error.to_string())),
        Err(_) => {
            return Ok(LiveVerification::Unavailable(format!(
                "query.run exceeded {:.3} s",
                deadline.as_secs_f64()
            )))
        }
    };
    let live: RunView = serde_json::from_value(result)
        .context("live query.run returned an invalid derived projection")?;
    verify_rebuild_matches_live(rebuilt, &live)
        .map_err(|difference| exit_failure(1, difference.to_string()))?;
    Ok(LiveVerification::Equal)
}
