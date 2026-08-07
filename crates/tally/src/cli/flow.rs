use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;

use tally_core::config::{FlowRegistration, PoolPredicate};
use tally_flow::validate_flow_pool_predicates;

use super::*;

#[derive(Debug, Default)]
pub(super) struct InheritedFlowEnvironment {
    pub(super) task_uuid: Option<String>,
    pub(super) job_id: Option<String>,
    pub(super) job_token: Option<String>,
    pub(super) brief_path: Option<PathBuf>,
    pub(super) brief_hash: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct InvocationEnvironment {
    pub(super) rpc_timeout: Option<OsString>,
    pub(super) result_projection_timeout: Option<OsString>,
    pub(super) flow_runner: Option<InheritedFlowEnvironment>,
}

impl InvocationEnvironment {
    pub(super) fn capture(opts: &Opts) -> Self {
        let flow_runner = matches!(
            &opts.command,
            Some(Command::Flow {
                command: FlowCommand::Run(_)
            })
        )
        .then(|| InheritedFlowEnvironment {
            task_uuid: std::env::var("TALLY_TASK_UUID").ok(),
            job_id: std::env::var("TALLY_JOB_ID").ok(),
            job_token: std::env::var("TALLY_JOB_TOKEN").ok(),
            brief_path: std::env::var_os("TALLY_BRIEF").map(PathBuf::from),
            brief_hash: std::env::var("TALLY_BRIEF_HASH").ok(),
        });
        Self {
            rpc_timeout: std::env::var_os(RPC_TIMEOUT_ENV),
            result_projection_timeout: std::env::var_os(RESULT_PROJECTION_TIMEOUT_ENV),
            flow_runner,
        }
    }
}

pub(super) fn configuration_contract() -> Value {
    json!({
        "configuration": "valid",
        "priorityRanks": {
            "interrupt": Priority::Interrupt.rank(),
            "high": Priority::High.rank(),
            "medium": Priority::Medium.rank(),
            "low": Priority::Low.rank(),
        }
    })
}

pub(super) struct JsonlLifecycleSink;

impl LifecycleSink for JsonlLifecycleSink {
    fn emit(&self, event: Value) -> Result<(), FlowError> {
        let line = serde_json::to_string(&event).map_err(|error| {
            FlowError::new(
                "FlowCaptureError",
                "lifecycle-serialization",
                format!("cannot serialize lifecycle event: {error}"),
            )
            .at(SourceLocation::new(1, 1))
        })?;
        writeln!(std::io::stdout().lock(), "{line}").map_err(|error| {
            FlowError::new(
                "FlowCaptureError",
                "lifecycle-write",
                format!("cannot write lifecycle event: {error}"),
            )
            .at(SourceLocation::new(1, 1))
        })
    }
}

pub(super) async fn run_flow(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
    command: FlowCommand,
    inherited: InheritedFlowEnvironment,
    result_projection_timeout: Option<OsString>,
) -> Result<()> {
    match command {
        FlowCommand::Cancel(args) => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "queue.cancel",
                Some(json!({"flowRunId": args.flow_run_id})),
            )
            .await
        }
        FlowCommand::Supersede(args) => {
            print_rpc(
                socket,
                config_path,
                rpc_timeout,
                "flow.supersede",
                Some(json!({
                    "flowRunId": args.flow_run_id,
                    "successorFlowRunId": args.new_flow_run_id,
                    "reason": args.reason.as_str(),
                })),
            )
            .await
        }
        FlowCommand::Check(args) => {
            let source = std::fs::read_to_string(&args.script)
                .with_context(|| format!("cannot read flow script {}", args.script.display()))?;
            let supplied_args = match (args.args, args.args_path) {
                (Some(args), None) => Some(args),
                (None, Some(path)) => Some(load_flow_args(&path)?),
                (None, None) => None,
                (Some(_), Some(_)) => unreachable!("clap rejects conflicting flow args inputs"),
            };
            let catalog = args
                .catalog
                .as_deref()
                .map(load_catalog)
                .transpose()
                .map_err(flow_error)?;
            let checked = check_script(
                &source,
                Some(&args.script),
                CheckOptions {
                    args: supplied_args.as_ref(),
                    catalog: catalog.as_ref().map(|(catalog, _)| catalog),
                    catalog_hash: catalog.as_ref().map(|(_, hash)| hash.as_str()),
                },
            )
            .map_err(flow_error)?;
            if let Some(config_path) = config_path {
                let config = load_client_config(Some(config_path))?;
                let windowed_consumption_pools = config
                    .pools
                    .iter()
                    .filter(|(_, pool)| {
                        matches!(pool.predicate, PoolPredicate::WindowedConsumption(_))
                    })
                    .map(|(name, _)| name.clone())
                    .collect::<BTreeSet<_>>();
                validate_flow_pool_predicates(&checked.meta, &windowed_consumption_pools)
                    .map_err(flow_error)?;
            }
            outln!("{}", serde_json::to_string(&checked.meta_json)?);
            Ok(())
        }
        FlowCommand::Run(args) => {
            if args.rpc_call_deadline_sec == Some(0) {
                return Err(invalid("--rpc-call-deadline-sec must be greater than zero"));
            }
            let rpc_call_timeout = args.rpc_call_deadline_sec.map(Duration::from_secs);
            let projection_wait = resolve_result_projection_timeout(
                args.result_projection_wait_ms,
                result_projection_timeout,
            )?;
            let source = std::fs::read_to_string(&args.script)
                .with_context(|| format!("cannot read flow script {}", args.script.display()))?;
            let InheritedFlowEnvironment {
                task_uuid: inherited_task_uuid,
                job_id: inherited_job_id,
                job_token: inherited_job_token,
                brief_path,
                brief_hash,
            } = inherited;
            let flow_args = match (args.args, args.args_path, args.args_from_brief) {
                (Some(args), None, false) => args,
                (None, Some(path), false) => load_flow_args(&path)?,
                (None, None, true) => {
                    let path = brief_path
                        .ok_or_else(|| invalid("--args-from-brief requires TALLY_BRIEF"))?;
                    let hash = brief_hash
                        .ok_or_else(|| invalid("--args-from-brief requires TALLY_BRIEF_HASH"))?;
                    load_verified_flow_args(&path, &hash)?
                }
                (None, None, false) => json!({}),
                _ => unreachable!("clap rejects conflicting flow args inputs"),
            };
            let flow_run_id = args
                .flow_run_id
                .or_else(|| inherited_task_uuid.clone())
                .ok_or_else(|| {
                    flow_error(
                        FlowError::new(
                            "FlowStartupError",
                            "flow-run-id-missing",
                            "flow run requires --flow-run-id or TALLY_TASK_UUID",
                        )
                        .at(SourceLocation::new(1, 1)),
                    )
                })?;
            uuid::Uuid::parse_str(&flow_run_id).map_err(|_| {
                flow_error(FlowError::new(
                    "FlowStartupError",
                    "flow-run-id-invalid",
                    format!("flow run ID {flow_run_id:?} is not a UUID"),
                ))
            })?;
            let runner = captured_runner_identity(
                inherited_task_uuid,
                inherited_job_id,
                inherited_job_token,
            )
            .map_err(|error| flow_error(*error))?;
            let catalog = args
                .catalog
                .as_deref()
                .map(load_catalog)
                .transpose()
                .map_err(flow_error)?;
            let mut options = RunOptions::new(flow_run_id, flow_args);
            options.max_nodes = args.max_nodes;
            if let Some((catalog, hash)) = catalog {
                options.catalog = Some(catalog);
                options.catalog_hash = Some(hash);
            }
            let client_config = load_client_config(config_path)?;
            if runner.task_uuid.is_none() {
                if let Some((flow, workload_mutex)) =
                    matching_workload_mutex(&client_config.flows, &args.script)
                {
                    return Err(flow_error(
                        FlowError::new(
                            "FlowStartupError",
                            "workload-mutex-parent-required",
                            format!(
                                "flow {flow:?} declares workloadMutex {workload_mutex:?}; run it through an admitted parent job holding pools \"flow\" and {workload_mutex:?}"
                            ),
                        )
                        .at(SourceLocation::new(1, 1))
                        .detail("flow", flow)
                        .detail("workloadMutex", workload_mutex),
                    ));
                }
            }
            let max_frame_bytes = client_config.max_frame_bytes;
            let final_message_adapters = client_config
                .adapters
                .iter()
                .filter(|(_, adapter)| adapter.scrape.contains_key("finalMessage"))
                .map(|(name, _)| name.clone())
                .collect();
            options.adapter_skill_revisions = client_config
                .adapters
                .iter()
                .filter_map(|(name, adapter)| {
                    adapter
                        .resolved_skill_revision()
                        .map(|revision| (name.clone(), revision))
                })
                .collect();
            options.pool_credentials = client_config
                .pools
                .into_iter()
                .map(|(name, pool)| (name, pool.credentials))
                .collect();
            ensure_sanitized_flow_environment()?;
            let socket = socket.to_owned();
            let script = args.script;
            let runtime = tokio::runtime::Handle::current();
            let outcome = tokio::task::spawn_blocking(move || {
                let _runtime = runtime.enter();
                let lifecycle: Rc<dyn LifecycleSink> = Rc::new(JsonlLifecycleSink);
                let mut client = LiveFlowClient::new(socket, max_frame_bytes, runner)
                    .with_final_message_adapters(final_message_adapters);
                if let Some(timeout) = rpc_call_timeout {
                    client = client.with_call_timeout(timeout);
                }
                if let Some(timeout) = projection_wait {
                    client = client.with_result_projection_timeout(timeout);
                }
                let client = client.with_lifecycle_sink(Rc::clone(&lifecycle));
                run_script(&source, Some(&script), Rc::new(client), lifecycle, options)
                    .map_err(Box::new)
            })
            .await
            .context("flow runner worker failed")?;
            match outcome {
                Ok(report) => {
                    JsonlLifecycleSink
                        .emit(json!({"type": "flow-report", "report": report}))
                        .map_err(flow_error)?;
                    Ok(())
                }
                Err(error) => {
                    JsonlLifecycleSink
                        .emit(json!({
                            "type": "flow-failed",
                            "error": error.report(),
                        }))
                        .map_err(flow_error)?;
                    Err(flow_error(*error))
                }
            }
        }
    }
}

/// Resolve the projection wait from `--result-projection-wait-ms` and
/// `TALLY_RESULT_PROJECTION_TIMEOUT_MS` (#432), flag first.
///
/// Neither set keeps the client's 10 s default. A set value must be a positive
/// whole number of milliseconds: widening the window is how an operator who
/// knows the daemon stalls out-waits the stall instead of losing a node whose
/// exit evidence already passed. An unparsable or zero value is refused loudly
/// rather than silently falling back to the default, and the flag wins over the
/// environment — both rules mirror `--rpc-timeout-sec`/`TALLY_RPC_TIMEOUT_SEC`.
///
/// The flag is the seam that reaches a campaign. A campaign pass runs as a
/// daemon-launched transient unit whose environment is built from an explicit
/// `--setenv` list, so an operator's shell environment never reaches it;
/// `campaign arm --projection-wait-ms` records the value and `dispatch_campaign`
/// puts it on this flag. The environment channel is for a `tally flow run` the
/// operator launches themselves.
fn resolve_result_projection_timeout(
    flag: Option<u64>,
    value: Option<OsString>,
) -> Result<Option<Duration>> {
    if let Some(millis) = flag {
        if millis == 0 {
            return Err(invalid(
                "--result-projection-wait-ms must be greater than zero",
            ));
        }
        return Ok(Some(Duration::from_millis(millis)));
    }
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value.to_str().ok_or_else(|| {
        invalid(format!(
            "{RESULT_PROJECTION_TIMEOUT_ENV} must be valid UTF-8"
        ))
    })?;
    let millis = text.parse::<u64>().map_err(|_| {
        invalid(format!(
            "{RESULT_PROJECTION_TIMEOUT_ENV} must be a whole number of milliseconds"
        ))
    })?;
    if millis == 0 {
        return Err(invalid(format!(
            "{RESULT_PROJECTION_TIMEOUT_ENV} must be greater than zero"
        )));
    }
    Ok(Some(Duration::from_millis(millis)))
}

fn load_flow_args(path: &Path) -> Result<Value> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("cannot open flow arguments {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("cannot inspect flow arguments {}", path.display()))?;
    if !metadata.is_file() {
        return Err(invalid(format!(
            "flow arguments {} are not a regular file",
            path.display()
        )));
    }
    if metadata.len() > tally_core::brief::MAX_BRIEF_BYTES {
        return Err(invalid(format!(
            "flow arguments {} exceed the {}-byte input limit",
            path.display(),
            tally_core::brief::MAX_BRIEF_BYTES
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(tally_core::brief::MAX_BRIEF_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("cannot read flow arguments {}", path.display()))?;
    if bytes.len() as u64 > tally_core::brief::MAX_BRIEF_BYTES {
        return Err(invalid(format!(
            "flow arguments {} grew beyond the {}-byte input limit",
            path.display(),
            tally_core::brief::MAX_BRIEF_BYTES
        )));
    }
    serde_json::from_slice(&bytes)
        .with_context(|| format!("flow arguments {} are not valid JSON", path.display()))
}

fn load_verified_flow_args(path: &Path, hash: &str) -> Result<Value> {
    tally_core::brief::read_verified(path, hash)
        .map(|prepared| prepared.document().clone())
        .with_context(|| {
            format!(
                "cannot verify flow arguments {} against TALLY_BRIEF_HASH",
                path.display()
            )
        })
}

fn matching_workload_mutex<'a>(
    flows: &'a BTreeMap<String, FlowRegistration>,
    script: &Path,
) -> Option<(&'a str, &'a str)> {
    flows.iter().find_map(|(name, registration)| {
        let workload_mutex = registration.workload_mutex.as_deref()?;
        same_script(script, &registration.script).then_some((name.as_str(), workload_mutex))
    })
}

fn same_script(left: &Path, right: &Path) -> bool {
    left == right
        || matches!(
            (std::fs::canonicalize(left), std::fs::canonicalize(right)),
            (Ok(left), Ok(right)) if left == right
        )
}

pub(super) fn captured_runner_identity(
    task_uuid: Option<String>,
    job_id: Option<String>,
    job_token: Option<String>,
) -> std::result::Result<RunnerIdentity, Box<FlowError>> {
    match (task_uuid, job_id) {
        (None, None) => Ok(RunnerIdentity {
            job_token,
            ..RunnerIdentity::default()
        }),
        (Some(task_uuid), Some(job_id)) => {
            for (name, value) in [("TALLY_TASK_UUID", &task_uuid), ("TALLY_JOB_ID", &job_id)] {
                uuid::Uuid::parse_str(value).map_err(|_| {
                    Box::new(FlowError::new(
                        "FlowStartupError",
                        "runner-identity-invalid",
                        format!("{name}={value:?} is not a UUID"),
                    ))
                })?;
            }
            Ok(RunnerIdentity {
                task_uuid: Some(task_uuid),
                job_id: Some(job_id),
                job_token,
                related_trigger: None,
            })
        }
        _ => Err(Box::new(FlowError::new(
            "FlowStartupError",
            "runner-identity-incomplete",
            "TALLY_TASK_UUID and TALLY_JOB_ID must either both be set or both be absent",
        ))),
    }
}

pub(super) fn sanitize_inherited_tally_environment() {
    let inherited = std::env::vars_os()
        .filter_map(|(name, _)| {
            name.to_str()
                .filter(|name| name.starts_with("TALLY_") && *name != "TALLY_SOCKET")
                .map(ToOwned::to_owned)
        })
        .collect::<Vec<_>>();
    for name in inherited {
        std::env::remove_var(name);
    }
}

pub(super) fn ensure_sanitized_flow_environment() -> Result<()> {
    if let Some(name) = std::env::vars_os().find_map(|(name, _)| {
        name.to_str()
            .filter(|name| name.starts_with("TALLY_") && *name != "TALLY_SOCKET")
            .map(ToOwned::to_owned)
    }) {
        return Err(anyhow::anyhow!(
            "flow worker inherited reserved environment variable {name}"
        ));
    }
    Ok(())
}

pub(super) fn flow_error(error: FlowError) -> anyhow::Error {
    let code = match error.code.as_str() {
        "replay-divergence"
        | "script-changed-mid-run"
        | "args-changed-mid-run"
        | "catalog-changed-mid-run"
        | "flow-run-superseded" => 20,
        "flow-cancelled" => 4,
        "flow-run-id-missing"
        | "flow-run-id-invalid"
        | "runner-identity-invalid"
        | "runner-identity-incomplete"
        | "workload-mutex-parent-required" => 2,
        "script-syntax"
        | "script-encoding"
        | "script-evaluation"
        | "script-exception"
        | "unhandled-rejection"
        | "determinism-violation"
        | "iteration-cap"
        | "runtime-limit"
        | "microtask-budget"
        | "wall-clock-budget" => 10,
        _ => 1,
    };
    let message = serde_json::to_string(&error.report()).unwrap_or_else(|_| error.to_string());
    anyhow::Error::new(ExitFailure { code, message })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #432 acceptance 2: the projection wait is configurable through
    /// `--result-projection-wait-ms` and `TALLY_RESULT_PROJECTION_TIMEOUT_MS`.
    /// Neither set keeps the client default (the caller does not override); a
    /// positive value widens the window; a zero or unparsable value is refused
    /// loudly rather than silently falling back; and the flag wins over the
    /// environment, because the flag is the channel a campaign dispatch uses.
    #[test]
    fn result_projection_timeout_override_is_parsed_and_refused_loudly() {
        assert_eq!(resolve_result_projection_timeout(None, None).unwrap(), None);
        assert_eq!(
            resolve_result_projection_timeout(None, Some(OsString::from("300000"))).unwrap(),
            Some(Duration::from_millis(300_000))
        );
        assert_eq!(
            resolve_result_projection_timeout(None, Some(OsString::from("1"))).unwrap(),
            Some(Duration::from_millis(1))
        );
        assert!(resolve_result_projection_timeout(None, Some(OsString::from("0"))).is_err());
        assert!(resolve_result_projection_timeout(None, Some(OsString::from("10s"))).is_err());
        assert!(resolve_result_projection_timeout(None, Some(OsString::from("-5"))).is_err());
        assert!(resolve_result_projection_timeout(None, Some(OsString::from(""))).is_err());

        assert_eq!(
            resolve_result_projection_timeout(Some(240_000), None).unwrap(),
            Some(Duration::from_millis(240_000))
        );
        assert!(resolve_result_projection_timeout(Some(0), None).is_err());
        // The flag wins, and it wins even over an environment value the
        // environment parser would have refused.
        assert_eq!(
            resolve_result_projection_timeout(Some(240_000), Some(OsString::from("5"))).unwrap(),
            Some(Duration::from_millis(240_000))
        );
        assert_eq!(
            resolve_result_projection_timeout(Some(240_000), Some(OsString::from("nonsense")))
                .unwrap(),
            Some(Duration::from_millis(240_000))
        );
    }
}
