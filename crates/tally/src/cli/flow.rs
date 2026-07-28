use std::collections::{BTreeMap, BTreeSet};

use tally_core::config::{FlowRegistration, PoolPredicate};
use tally_flow::validate_flow_pool_predicates;

use super::*;

#[derive(Debug, Default)]
pub(super) struct InheritedFlowEnvironment {
    pub(super) task_uuid: Option<String>,
    pub(super) job_id: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct InvocationEnvironment {
    pub(super) rpc_timeout: Option<OsString>,
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
        });
        Self {
            rpc_timeout: std::env::var_os(RPC_TIMEOUT_ENV),
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
        FlowCommand::Check(args) => {
            let source = std::fs::read_to_string(&args.script)
                .with_context(|| format!("cannot read flow script {}", args.script.display()))?;
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
                    args: args.args.as_ref(),
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
            println!("{}", serde_json::to_string(&checked.meta_json)?);
            Ok(())
        }
        FlowCommand::Run(args) => {
            if args.rpc_call_deadline_sec == Some(0) {
                return Err(invalid("--rpc-call-deadline-sec must be greater than zero"));
            }
            let rpc_call_timeout = args.rpc_call_deadline_sec.map(Duration::from_secs);
            let source = std::fs::read_to_string(&args.script)
                .with_context(|| format!("cannot read flow script {}", args.script.display()))?;
            let inherited_task_uuid = inherited.task_uuid;
            let inherited_job_id = inherited.job_id;
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
            let runner = captured_runner_identity(inherited_task_uuid, inherited_job_id)
                .map_err(|error| flow_error(*error))?;
            let catalog = args
                .catalog
                .as_deref()
                .map(load_catalog)
                .transpose()
                .map_err(flow_error)?;
            let mut options = RunOptions::new(flow_run_id, args.args);
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
) -> std::result::Result<RunnerIdentity, Box<FlowError>> {
    match (task_uuid, job_id) {
        (None, None) => Ok(RunnerIdentity::default()),
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
        | "catalog-changed-mid-run" => 20,
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
