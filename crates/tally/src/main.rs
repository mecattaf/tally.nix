use std::error::Error as StdError;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};
use tally_core::config::Priority;
use tally_core::daemon::{Daemon, DaemonPaths, DaemonSettings};
use tally_core::evidence::RetryPolicy;
use tally_core::executor::{persist_exit_record_from_env, ExecutionPaths, UnitLimits};
use tally_core::producers::{GhCliIntake, GhObservation, ProducerEngine, ProducerObservation};
use tally_core::recovery::RecoveryPolicy;
use tally_core::taskdb::EnqueueSource;
use tally_core::wire::{EnqueuePayload, RpcClient, WireErrorCode, WireIoError};
use tally_core::witness::{append_attestation, verify_attestations, verify_file};
use tally_core::{
    adapters::{AdapterEngine, ScrapeResult},
    Config,
};

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Mode {
    Daemon,
    CheckConfig,
}

#[derive(Debug, Parser)]
#[command(
    name = "tally",
    version,
    about = "Contention and proof for impure labor"
)]
struct Opts {
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    #[command(name = "__record-unit-exit", hide = true)]
    RecordUnitExit(RecordUnitExitArgs),
    #[command(name = "__adapter-render", hide = true)]
    AdapterRender(AdapterRenderArgs),
    #[command(name = "__producer-dispatch", hide = true)]
    ProducerDispatch(ProducerDispatchArgs),
    Enqueue(EnqueueArgs),
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    Witness {
        #[command(subcommand)]
        command: WitnessCommand,
    },
    Lease {
        #[command(subcommand)]
        command: LeaseCommand,
    },
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    Query {
        #[command(subcommand)]
        command: QueryCommand,
    },
}

#[derive(Debug, Args)]
struct AdapterRenderArgs {
    adapter: String,
    #[arg(long)]
    captures: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        requires = "scrape_stderr",
        conflicts_with = "captures"
    )]
    scrape_stdout: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        requires = "scrape_stdout",
        conflicts_with = "captures"
    )]
    scrape_stderr: Option<PathBuf>,
    #[arg(last = true)]
    argv: Vec<String>,
}

#[derive(Debug, Args)]
struct ProducerDispatchArgs {
    producer: String,
    #[arg(long)]
    event: String,
    #[arg(long, value_name = "PATH")]
    state_dir: Option<PathBuf>,
    #[arg(long, hide = true)]
    engine_only: bool,
}

#[derive(Debug, Args)]
struct RecordUnitExitArgs {
    #[arg(long, value_name = "PATH")]
    record: PathBuf,
    #[arg(long)]
    unit: String,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliPriority {
    Interrupt,
    High,
    Medium,
    Low,
}

impl From<CliPriority> for Priority {
    fn from(value: CliPriority) -> Self {
        match value {
            CliPriority::Interrupt => Self::Interrupt,
            CliPriority::High => Self::High,
            CliPriority::Medium => Self::Medium,
            CliPriority::Low => Self::Low,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliSource {
    Manual,
    Orchestrator,
    Calendar,
    EventsDir,
    Gh,
    BuildEffect,
    PoolReachability,
}

impl From<CliSource> for EnqueueSource {
    fn from(value: CliSource) -> Self {
        match value {
            CliSource::Manual => Self::Manual,
            CliSource::Orchestrator => Self::Orchestrator,
            CliSource::Calendar => Self::Calendar,
            CliSource::EventsDir => Self::EventsDir,
            CliSource::Gh => Self::Gh,
            CliSource::BuildEffect => Self::BuildEffect,
            CliSource::PoolReachability => Self::PoolReachability,
        }
    }
}

#[derive(Debug, Args)]
struct EnqueueArgs {
    #[arg(long = "pool", required = true, action = clap::ArgAction::Append)]
    pools: Vec<String>,
    #[arg(long, value_enum, default_value = "medium")]
    priority: CliPriority,
    #[arg(long, default_value = "shell")]
    adapter: String,
    #[arg(long, value_enum, default_value = "manual")]
    source: CliSource,
    #[arg(long)]
    dedup_key: Option<String>,
    #[arg(long)]
    parent: Option<String>,
    #[arg(long)]
    invocation: Option<String>,
    #[arg(long = "evidence", action = clap::ArgAction::Append)]
    evidence: Vec<String>,
    #[arg(long, value_parser = parse_opaque_json, allow_hyphen_values = true)]
    evidence_class: Option<Value>,
    #[arg(long, allow_hyphen_values = true)]
    manifest_hash: Option<String>,
    #[arg(long)]
    consumption_estimate: Option<u64>,
    #[arg(long)]
    runtime_max_sec: Option<u64>,
    #[arg(long)]
    no_enqueue: bool,
    #[arg(long)]
    wait: bool,
    #[arg(last = true)]
    argv: Vec<String>,
}

fn parse_opaque_json(value: &str) -> Result<Value, String> {
    serde_json::from_str(value).map_err(|error| format!("invalid JSON value: {error}"))
}

#[derive(Debug, Subcommand)]
enum QueueCommand {
    Enqueue(Box<EnqueueArgs>),
    Cancel {
        job: String,
        #[arg(long)]
        force: bool,
    },
    Pause {
        pool: Option<String>,
        #[arg(long)]
        all: bool,
    },
    Resume {
        pool: Option<String>,
        #[arg(long)]
        all: bool,
    },
    Drain,
    AwaitJob {
        job: String,
    },
    AwaitBarrier {
        barrier: String,
    },
}

#[derive(Debug, Subcommand)]
enum WitnessCommand {
    Append {
        #[arg(long, value_name = "PATH")]
        ledger: Option<PathBuf>,
        #[arg(long)]
        payload: String,
    },
    Verify {
        #[arg(value_name = "PATH", conflicts_with = "ledger")]
        path: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        ledger: Option<PathBuf>,
        #[arg(long)]
        attestations: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand)]
enum LeaseCommand {
    Acquire {
        #[arg(required = true, num_args = 1..)]
        pools: Vec<String>,
    },
    Release {
        lease: String,
    },
    Status {
        lease: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCommand {
    Run {
        #[arg(long)]
        mock: bool,
        #[arg(long)]
        cpu_weight: Option<u16>,
        #[arg(long)]
        memory_max_bytes: Option<u64>,
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
        #[arg(long, default_value_t = 20)]
        yield_grace_sec: u64,
    },
    Drain,
}

#[derive(Debug, Subcommand)]
enum QueryCommand {
    Status {
        #[arg(long)]
        pool: Option<String>,
    },
    Log {
        #[arg(long)]
        task: Option<String>,
    },
    Render {
        #[arg(long, default_value = "text")]
        format: String,
    },
    Standup {
        #[arg(long)]
        since: Option<String>,
    },
    Pools,
}

#[derive(Debug)]
struct ExitFailure {
    code: i32,
    message: String,
}

impl std::fmt::Display for ExitFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl StdError for ExitFailure {}

fn invalid(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ExitFailure {
        code: 2,
        message: message.into(),
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let recorder_mode =
        std::env::args_os().nth(1).as_deref() == Some(OsStr::new("__record-unit-exit"));
    match run().await {
        Ok(()) => {}
        Err(error) => {
            if !recorder_mode {
                eprintln!("tally: {error:#}");
            }
            std::process::exit(error_exit_code(&error));
        }
    }
}

async fn run() -> Result<()> {
    let mut args = std::env::args_os().collect::<Vec<_>>();
    let invoked_as_tallyd =
        args.first().and_then(|arg| Path::new(arg).file_name()) == Some(OsStr::new("tallyd"));
    if invoked_as_tallyd && args.len() == 1 {
        args.extend(["daemon".into(), "run".into()]);
    }
    execute(Opts::parse_from(args)).await
}

fn configuration_contract() -> Value {
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

async fn execute(opts: Opts) -> Result<()> {
    if matches!(opts.mode, Some(Mode::CheckConfig)) {
        let path = opts
            .config
            .context("--mode check-config requires --config PATH")?;
        Config::from_path(&path)?;
        println!("{}", serde_json::to_string(&configuration_contract())?);
        return Ok(());
    }

    let socket = opts.socket.unwrap_or_else(default_socket_path);
    match opts.command {
        Some(Command::RecordUnitExit(args)) => {
            persist_exit_record_from_env(&args.record, &args.unit)?;
            Ok(())
        }
        Some(Command::AdapterRender(args)) => {
            let config_path = opts
                .config
                .context("__adapter-render requires --config PATH")?;
            let config = Config::from_path(&config_path)?;
            let engine = AdapterEngine::new(&config.adapters);
            let (invocation, scraped) = if let Some(captures) = args.captures {
                let captures = ScrapeResult {
                    captures: serde_json::from_str(&captures)
                        .context("--captures must be a JSON object")?,
                };
                (engine.resume(&args.adapter, &args.argv, &captures)?, None)
            } else if let (Some(stdout), Some(stderr)) = (args.scrape_stdout, args.scrape_stderr) {
                let captures = engine.scrape_paths(
                    &args.adapter,
                    &ExecutionPaths {
                        stdout,
                        stderr,
                        exit_record: PathBuf::from("unused"),
                        capture_generation: PathBuf::from("unused"),
                    },
                )?;
                let invocation = engine.resume(&args.adapter, &args.argv, &captures)?;
                (invocation, Some(captures.captures))
            } else {
                (engine.launch(&args.adapter, &args.argv)?, None)
            };
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "argv": invocation.argv,
                    "env": invocation.env,
                    "yieldHook": invocation.yield_hook,
                    "captures": scraped,
                }))?
            );
            Ok(())
        }
        Some(Command::ProducerDispatch(args)) => {
            run_producer_dispatch(opts.config, &socket, args).await
        }
        Some(Command::Daemon {
            command: DaemonCommand::Run { mock: true, .. },
        }) => {
            println!("tally mock daemon ready");
            Ok(())
        }
        Some(Command::Daemon {
            command:
                DaemonCommand::Run {
                    mock: false,
                    cpu_weight,
                    memory_max_bytes,
                    state_dir,
                    data_dir,
                    yield_grace_sec,
                },
        }) => {
            run_daemon_runtime(
                opts.config,
                socket,
                cpu_weight,
                memory_max_bytes,
                state_dir,
                data_dir,
                yield_grace_sec,
            )
            .await
        }
        None if matches!(opts.mode, Some(Mode::Daemon)) => {
            run_daemon_runtime(opts.config, socket, None, None, None, None, 20).await
        }
        Some(Command::Daemon {
            command: DaemonCommand::Drain,
        }) => print_rpc(&socket, "queue.drain", Some(json!({}))).await,
        Some(Command::Enqueue(args)) => run_enqueue(&socket, args).await,
        Some(Command::Queue {
            command: QueueCommand::Enqueue(args),
        }) => run_enqueue(&socket, *args).await,
        Some(Command::Queue { command }) => run_queue(&socket, command).await,
        Some(Command::Witness { command }) => run_witness(command),
        Some(Command::Lease { command }) => run_lease(&socket, command).await,
        Some(Command::Query { command }) => run_query(&socket, command).await,
        None => {
            Opts::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

async fn run_producer_dispatch(
    config_path: Option<PathBuf>,
    socket: &Path,
    args: ProducerDispatchArgs,
) -> Result<()> {
    let config_path = config_path.context("__producer-dispatch requires --config PATH")?;
    let config = Config::from_path(&config_path)?;
    let event: ProducerObservation = serde_json::from_str(&args.event)
        .context("--event must be a producer observation JSON object")?;
    let state_dir = args.state_dir.map_or_else(default_state_dir, Ok)?;
    let events_dir = state_dir.join("events");
    let engine = ProducerEngine::new(&config.producers, &events_dir, &state_dir);
    let expected_kind = match &event {
        ProducerObservation::Calendar => "calendar",
        ProducerObservation::EventsDir => "events-dir",
        ProducerObservation::Gh { .. } => "gh",
        ProducerObservation::BuildEffect { .. } => "build-effect",
        ProducerObservation::PoolReachability { .. } => "pool-reachability",
    };
    let actual_kind = engine.producer_kind(&args.producer)?;
    if actual_kind != expected_kind {
        bail!(
            "producer {:?} has kind {:?}, but the observation has kind {:?}",
            args.producer,
            actual_kind,
            expected_kind
        );
    }
    let now = Utc::now();
    let result = match event {
        ProducerObservation::Calendar => {
            serde_json::to_value(engine.emit_calendar(&args.producer, now)?)?
        }
        ProducerObservation::EventsDir => {
            let mut client = RpcClient::connect(socket).await?;
            client.call("queue.drain", Some(json!({}))).await?
        }
        ProducerObservation::Gh {
            source,
            item_id,
            actor,
            self_actor,
        } => match (source, item_id, actor, self_actor) {
            (None, None, None, None) => serde_json::to_value(engine.poll_gh(
                &args.producer,
                &GhCliIntake::default(),
                now,
            )?)?,
            (Some(source), Some(item_id), Some(actor), Some(self_actor)) => {
                serde_json::to_value(engine.emit_gh(
                    &args.producer,
                    &GhObservation {
                        source,
                        item_id,
                        actor,
                        self_actor,
                    },
                    now,
                )?)?
            }
            _ => bail!(
                "gh observation requires either no fields for a configured poll or all of source, itemId, actor, and selfActor"
            ),
        },
        ProducerObservation::BuildEffect { store_path } => {
            let store_paths = if let Some(store_path) = store_path {
                vec![store_path]
            } else {
                engine.scan_build_effect(&args.producer)?
            };
            let outcomes = store_paths
                .iter()
                .map(|store_path| engine.emit_build_effect(&args.producer, store_path, now))
                .collect::<Result<Vec<_>, _>>()?;
            serde_json::to_value(outcomes)?
        }
        ProducerObservation::PoolReachability { reachable } => {
            let outcome = engine.observe_reachability(&args.producer, reachable, now)?;
            if let Some(transition) = outcome.transition {
                if args.engine_only {
                    engine
                        .acknowledge_reachability_transition(&args.producer, outcome.generation)?;
                } else {
                    let mut client = RpcClient::connect(socket).await?;
                    client
                        .call(
                            "__producer.pool-transition",
                            Some(json!({
                                "producer": args.producer,
                                "transition": transition,
                                "generation": outcome.generation,
                            })),
                        )
                        .await?;
                    engine
                        .acknowledge_reachability_transition(&args.producer, outcome.generation)?;
                }
            }
            serde_json::to_value(outcome)?
        }
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon_runtime(
    config_path: Option<PathBuf>,
    socket: PathBuf,
    cpu_weight: Option<u16>,
    memory_max_bytes: Option<u64>,
    state_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    yield_grace_sec: u64,
) -> Result<()> {
    let config_path = config_path.map_or_else(default_config_path, Ok)?;
    let config = Config::from_path(&config_path)?;
    let cpu_weight = required_daemon_value(cpu_weight, "TALLY_CPU_WEIGHT", "--cpu-weight")?;
    let memory_max_bytes = required_daemon_value(
        memory_max_bytes,
        "TALLY_MEMORY_MAX_BYTES",
        "--memory-max-bytes",
    )?;
    let state_dir = state_dir.map_or_else(default_state_dir, Ok)?;
    let data_dir = data_dir.map_or_else(default_data_dir, Ok)?;
    let recorder_program = std::env::current_exe().context("cannot resolve tally executable")?;
    let daemon = Daemon::open(
        config,
        DaemonPaths {
            socket,
            state_dir,
            data_dir,
        },
        DaemonSettings {
            unit_limits: UnitLimits {
                cpu_weight,
                memory_max_bytes,
            },
            yield_grace: std::time::Duration::from_secs(yield_grace_sec),
            recovery_policy: RecoveryPolicy {
                retry: RetryPolicy {
                    auto_pool_return: true,
                    auto_resource_return: false,
                    auto_bounded_requeue: false,
                },
                max_attempts: 2,
            },
        },
        recorder_program,
    )
    .await?;
    daemon.run().await?;
    Ok(())
}

fn required_daemon_value<T>(
    cli: Option<T>,
    environment: &'static str,
    flag: &'static str,
) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    if let Some(value) = cli {
        return Ok(value);
    }
    let value = std::env::var(environment)
        .with_context(|| format!("daemon requires {flag} or {environment}"))?;
    value
        .parse()
        .map_err(|error| anyhow::anyhow!("{environment} has an invalid value: {error}"))
}

async fn run_enqueue(socket: &Path, mut args: EnqueueArgs) -> Result<()> {
    let has_invocation = args.invocation.is_some();
    let has_argv = !args.argv.is_empty();
    if has_invocation == has_argv {
        return Err(invalid(
            "enqueue requires exactly one of --invocation or -- <argv...>",
        ));
    }
    if args.runtime_max_sec == Some(0) {
        return Err(invalid("--runtime-max-sec must be positive"));
    }
    tally_core::poolset::canonicalize(&mut args.pools)
        .map_err(|error| invalid(error.to_string()))?;
    let payload = EnqueuePayload {
        invocation: args.invocation,
        argv: has_argv.then_some(args.argv),
        pools: Some(args.pools),
        priority: Some(args.priority.into()),
        adapter: Some(args.adapter),
        source: Some(args.source.into()),
        dedup_key: args.dedup_key,
        parent: args.parent,
        evidence: args.evidence,
        evidence_class: args.evidence_class,
        manifest_hash: args.manifest_hash,
        consumption_estimate: args.consumption_estimate,
        runtime_max_sec: args.runtime_max_sec,
        no_enqueue: args.no_enqueue,
        credentials: Default::default(),
        caller_job_id: std::env::var("TALLY_JOB_ID").ok(),
        gh_actor: std::env::var("TALLY_GH_ACTOR").ok(),
        gh_self_actor: std::env::var("TALLY_GH_SELF_ACTOR").ok(),
        gh_origin: None,
        wait: args.wait,
    };
    let mut client = RpcClient::connect(socket).await?;
    let result = client
        .call("queue.enqueue", Some(serde_json::to_value(payload)?))
        .await?;
    if !args.wait {
        println!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    if let Some(verdict) = result.get("verdict").and_then(Value::as_str) {
        println!("{}", serde_json::to_string(&result)?);
        let code = verdict_exit_code(verdict);
        if code != 0 {
            return Err(anyhow::Error::new(ExitFailure {
                code,
                message: format!("job finished with verdict {verdict}"),
            }));
        }
        return Ok(());
    }
    let key = if let Some(task_uuid) = result.get("task_uuid").filter(|value| !value.is_null()) {
        json!({"task_uuid": task_uuid})
    } else if let Some(job_id) = result.get("job_id").filter(|value| !value.is_null()) {
        json!({"job_id": job_id})
    } else {
        return Err(invalid(
            "queue.enqueue returned neither task_uuid nor job_id for --wait",
        ));
    };
    let waited = client.call("queue.await_job", Some(key)).await?;
    println!("{}", serde_json::to_string(&waited)?);
    let code = waited
        .get("verdict")
        .and_then(Value::as_str)
        .map(verdict_exit_code)
        .or_else(|| {
            waited
                .get("exit_code")
                .and_then(Value::as_i64)
                .map(|value| value.clamp(0, 255) as i32)
        })
        .unwrap_or(1);
    if code == 0 {
        Ok(())
    } else {
        Err(anyhow::Error::new(ExitFailure {
            code,
            message: "waited job returned a non-zero verdict".to_owned(),
        }))
    }
}

async fn run_queue(socket: &Path, command: QueueCommand) -> Result<()> {
    match command {
        QueueCommand::Enqueue(_) => unreachable!("enqueue is routed before run_queue"),
        QueueCommand::Cancel { job, force } => {
            print_rpc(
                socket,
                "queue.cancel",
                Some(json!({"task_uuid": job, "force": force})),
            )
            .await
        }
        QueueCommand::Pause { pool, all } => {
            print_rpc(
                socket,
                "queue.pause",
                Some(json!({"pool": pool, "all": all})),
            )
            .await
        }
        QueueCommand::Resume { pool, all } => {
            print_rpc(
                socket,
                "queue.resume",
                Some(json!({"pool": pool, "all": all})),
            )
            .await
        }
        QueueCommand::Drain => print_rpc(socket, "queue.drain", Some(json!({}))).await,
        QueueCommand::AwaitJob { job } => {
            print_rpc(socket, "queue.await_job", Some(json!({"task_uuid": job}))).await
        }
        QueueCommand::AwaitBarrier { barrier } => {
            print_rpc(
                socket,
                "queue.await_barrier",
                Some(json!({"barrier": barrier})),
            )
            .await
        }
    }
}

async fn run_lease(socket: &Path, command: LeaseCommand) -> Result<()> {
    match command {
        LeaseCommand::Acquire { mut pools } => {
            tally_core::poolset::canonicalize(&mut pools)
                .map_err(|error| invalid(error.to_string()))?;
            let pool = match pools.as_slice() {
                [pool] => Value::String(pool.clone()),
                pools => serde_json::to_value(pools)?,
            };
            print_rpc(socket, "lease.acquire", Some(json!({"pool": pool}))).await
        }
        LeaseCommand::Release { lease } => {
            print_rpc(socket, "lease.release", Some(json!({"lease": lease}))).await
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
            print_rpc(socket, "lease.status", Some(params)).await
        }
    }
}

async fn run_query(socket: &Path, command: QueryCommand) -> Result<()> {
    match command {
        QueryCommand::Status { pool } => {
            print_rpc(socket, "query.status", Some(json!({"pool": pool}))).await
        }
        QueryCommand::Log { task } => {
            print_rpc(socket, "query.log", Some(json!({"task": task}))).await
        }
        QueryCommand::Render { format } => {
            let mut client = RpcClient::connect(socket).await?;
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
            print_rpc(socket, "query.standup", Some(json!({"since": since}))).await
        }
        QueryCommand::Pools => print_rpc(socket, "query.pools", Some(json!({}))).await,
    }
}

async fn print_rpc(socket: &Path, method: &str, params: Option<Value>) -> Result<()> {
    let mut client = RpcClient::connect(socket).await?;
    let result = client.call(method, params).await?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn run_witness(command: WitnessCommand) -> Result<()> {
    match command {
        WitnessCommand::Append { ledger, payload } => {
            let path = ledger.unwrap_or(default_data_dir()?.join("attestations.jsonl"));
            let payload = serde_json::from_str(&payload).context("--payload must be valid JSON")?;
            let record = append_attestation(&path, payload)?;
            println!("{}", serde_json::to_string(&record)?);
            Ok(())
        }
        WitnessCommand::Verify {
            path,
            ledger,
            attestations,
        } => {
            let ledger = path
                .or(ledger)
                .unwrap_or(default_data_dir()?.join("witness.jsonl"));
            let attestations =
                attestations.unwrap_or_else(|| ledger.with_file_name("attestations.jsonl"));
            let verdict_report = verify_file(&ledger)?;
            let attestation_report = verify_attestations(&attestations)?;
            if verdict_report.ok {
                println!(
                    "verdict chain: ok ({} records, seq {:?}..{:?})",
                    verdict_report.records, verdict_report.first_seq, verdict_report.last_seq
                );
            } else {
                println!("verdict chain: invalid");
                for problem in &verdict_report.problems {
                    println!(
                        "line {} seq {:?} {:?}: {}",
                        problem.line, problem.seq, problem.kind, problem.reason
                    );
                }
            }
            println!(
                "attestation chain: {} ({} records; {})",
                if attestation_report.ok {
                    "ok"
                } else {
                    "invalid"
                },
                attestation_report.records,
                attestation_report.authentication
            );
            if !verdict_report.ok || !attestation_report.ok {
                bail!("ledger verification failed");
            }
            Ok(())
        }
    }
}

fn error_exit_code(error: &anyhow::Error) -> i32 {
    for cause in error.chain() {
        if let Some(failure) = cause.downcast_ref::<ExitFailure>() {
            return failure.code;
        }
        if let Some(wire) = cause.downcast_ref::<WireIoError>() {
            return match wire {
                WireIoError::Unreachable { .. } => 3,
                WireIoError::Rpc(WireErrorCode::InvalidParams, _, _) => 2,
                WireIoError::Rpc(WireErrorCode::NotFound, _, _) => 4,
                _ => 1,
            };
        }
    }
    1
}

fn verdict_exit_code(verdict: &str) -> i32 {
    match verdict {
        "pass" | "reused" => 0,
        "clean-exit-no-artifact" => 3,
        "cancelled" => 4,
        "failed" | "pool-vanished" | "preempted" | "runtime-exceeded" => 1,
        _ => 1,
    }
}

fn default_socket_path() -> PathBuf {
    if let Some(socket) = std::env::var_os("TALLY_SOCKET") {
        return PathBuf::from(socket);
    }
    std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || std::env::temp_dir().join("tally/tally.sock"),
        |runtime| PathBuf::from(runtime).join("tally/tally.sock"),
    )
}

fn default_config_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("tally/config.json"));
    }
    let home = std::env::var_os("HOME").context("HOME and XDG_CONFIG_HOME are both unset")?;
    Ok(PathBuf::from(home).join(".config/tally/config.json"))
}

fn default_state_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("tally"));
    }
    let home = std::env::var_os("HOME").context("HOME and XDG_STATE_HOME are both unset")?;
    Ok(PathBuf::from(home).join(".local/state/tally"))
}

fn default_data_dir() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("tally"));
    }
    let home = std::env::var_os("HOME").context("HOME and XDG_DATA_HOME are both unset")?;
    Ok(PathBuf::from(home).join(".local/share/tally"))
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[test]
    fn clap_tree_is_consistent() {
        Opts::command().debug_assert();
    }

    #[test]
    fn full_top_level_surface_is_visible() {
        let help = Opts::command().render_long_help().to_string();
        for verb in ["enqueue", "queue", "witness", "lease", "daemon", "query"] {
            assert!(help.contains(verb), "missing {verb} from help");
        }
        assert!(!help.contains("__record-unit-exit"));
    }

    #[test]
    fn hidden_exit_recorder_command_parses() {
        let options = Opts::try_parse_from([
            "tally",
            "__record-unit-exit",
            "--record",
            "/tmp/exit.json",
            "--unit",
            "tally-job-example.service",
        ])
        .unwrap();
        assert!(matches!(
            options.command,
            Some(Command::RecordUnitExit(RecordUnitExitArgs { record, unit }))
                if record.as_path() == Path::new("/tmp/exit.json")
                    && unit == "tally-job-example.service"
        ));
    }

    #[test]
    fn hidden_producer_dispatch_parses_a_typed_observation() {
        let options = Opts::try_parse_from([
            "tally",
            "--config",
            "/tmp/config.json",
            "__producer-dispatch",
            "health",
            "--event",
            r#"{"kind":"pool-reachability","reachable":false}"#,
            "--state-dir",
            "/tmp/state",
        ])
        .unwrap();
        assert!(matches!(
            options.command,
            Some(Command::ProducerDispatch(ProducerDispatchArgs {
                producer,
                state_dir: Some(state_dir),
                ..
            })) if producer == "health" && state_dir == Path::new("/tmp/state")
        ));
    }

    #[test]
    fn frozen_transport_exit_codes_are_stable() {
        let unreachable = anyhow::Error::new(WireIoError::Unreachable {
            path: PathBuf::from("/missing"),
            source: io::Error::from(io::ErrorKind::NotFound),
        });
        assert_eq!(error_exit_code(&unreachable), 3);
        for (wire_code, exit_code) in [
            (WireErrorCode::InvalidParams, 2),
            (WireErrorCode::NotFound, 4),
            (WireErrorCode::Internal, 1),
        ] {
            let error = anyhow::Error::new(WireIoError::Rpc(wire_code, "failure".to_owned(), None));
            assert_eq!(error_exit_code(&error), exit_code);
        }
    }

    #[test]
    fn waited_verdict_exit_codes_are_stable() {
        assert_eq!(verdict_exit_code("pass"), 0);
        assert_eq!(verdict_exit_code("reused"), 0);
        assert_eq!(verdict_exit_code("clean-exit-no-artifact"), 3);
        assert_eq!(verdict_exit_code("cancelled"), 4);
        for verdict in ["failed", "pool-vanished", "preempted", "runtime-exceeded"] {
            assert_eq!(verdict_exit_code(verdict), 1);
        }
    }

    #[test]
    fn enqueue_accepts_direct_argv_or_invocation() {
        let direct = Opts::try_parse_from([
            "tally",
            "enqueue",
            "--pool",
            "gpu",
            "--",
            "cmd",
            "two words",
        ]);
        assert!(direct.is_ok());
        let invocation = Opts::try_parse_from([
            "tally",
            "queue",
            "enqueue",
            "--pool",
            "gpu",
            "--invocation",
            "cmd 'two words'",
        ]);
        assert!(invocation.is_ok());
    }

    #[test]
    fn enqueue_accepts_opaque_evidence_metadata_flags() {
        let evidence_class = r#"{"arbitrary":[true,7,{"nested":null}]}"#;
        let options = Opts::try_parse_from([
            "tally",
            "enqueue",
            "--pool",
            "gpu",
            "--evidence-class",
            evidence_class,
            "--manifest-hash",
            "deliberately-not-validated://manifest value",
            "--",
            "true",
        ])
        .unwrap();
        let Some(Command::Enqueue(args)) = options.command else {
            panic!("expected enqueue command");
        };
        assert_eq!(
            args.evidence_class,
            Some(serde_json::from_str(evidence_class).unwrap())
        );
        assert_eq!(
            args.manifest_hash.as_deref(),
            Some("deliberately-not-validated://manifest value")
        );

        let scalar = Opts::try_parse_from([
            "tally",
            "enqueue",
            "--pool",
            "gpu",
            "--evidence-class",
            "-1",
            "--manifest-hash",
            "-opaque-manifest",
            "--",
            "true",
        ])
        .unwrap();
        let Some(Command::Enqueue(args)) = scalar.command else {
            panic!("expected enqueue command");
        };
        assert_eq!(args.evidence_class, Some(Value::from(-1)));
        assert_eq!(args.manifest_hash.as_deref(), Some("-opaque-manifest"));
    }
}
