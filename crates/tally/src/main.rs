mod flow_live;

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};
use tally_client::{
    default_config_path, resolve_max_frame_bytes, RpcClient, WireErrorCode, WireIoError,
    DEFAULT_MAX_FRAME_BYTES,
};
use tally_core::authorship::verify_authorship;
use tally_core::completion::{AcceptancePolicy, GateManifestSpec};
use tally_core::config::Priority;
use tally_core::daemon::{Daemon, DaemonPaths, DaemonSettings};
use tally_core::evidence::RetryPolicy;
use tally_core::exec_attestation::{
    compare as compare_witness_attestations, read_verified_exec_attestations, run_exec,
    ExecRunRequest, EXEC_ATTESTATION_LEDGER,
};
use tally_core::executor::{
    persist_exit_record_from_env, serve_remote_executor_stdio, ExecutionPaths, UnitLimits,
};
use tally_core::producers::{
    record_producer_runtime, GhCliAcknowledgementSink, GhCliIntake, GhObservation, ProducerEngine,
    ProducerObservation,
};
use tally_core::provenance::Orchestration;
use tally_core::recovery::RecoveryPolicy;
use tally_core::taskdb::{EnqueueSource, RelatedTrigger, WorkspaceMetadata, TASKDATA_DIRECTORY};
use tally_core::wire::EnqueuePayload;
use tally_core::witness::{
    append_attestation, read_verified_attestations, read_verified_records, GENESIS_PREV_HASH,
};
use tally_core::{
    adapters::{provisions_gate_manifest, AdapterEngine, AdapterJobOptions, ScrapeResult},
    Config,
};
use tally_flow::{
    check_script, load_catalog, run_script, CheckOptions, FlowError, LifecycleSink, RunOptions,
    SourceLocation,
};

use crate::flow_live::{LiveFlowClient, RunnerIdentity};

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
    #[command(name = "__remote-executor", hide = true)]
    RemoteExecutor,
    #[command(name = "__adapter-render", hide = true)]
    AdapterRender(AdapterRenderArgs),
    #[command(name = "__producer-dispatch", hide = true)]
    ProducerDispatch(ProducerDispatchArgs),
    Enqueue(Box<EnqueueArgs>),
    Gc(GcArgs),
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    Producer {
        #[command(subcommand)]
        command: ProducerCommand,
    },
    Witness {
        #[command(subcommand)]
        command: WitnessCommand,
    },
    View {
        #[command(subcommand)]
        command: ViewCommand,
    },
    Attest {
        #[command(subcommand)]
        command: AttestCommand,
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
    Flow {
        #[command(subcommand)]
        command: FlowCommand,
    },
}

#[derive(Debug, Subcommand)]
enum FlowCommand {
    Run(FlowRunArgs),
    Check(FlowCheckArgs),
}

#[derive(Debug, Args)]
struct FlowRunArgs {
    #[arg(value_name = "SCRIPT")]
    script: PathBuf,
    #[arg(long, default_value = "{}", value_parser = parse_opaque_json, allow_hyphen_values = true)]
    args: Value,
    #[arg(long, value_name = "PATH")]
    catalog: Option<PathBuf>,
    #[arg(long)]
    flow_run_id: Option<String>,
    #[arg(long, default_value_t = tally_flow::DEFAULT_MAX_NODES)]
    max_nodes: u32,
}

#[derive(Debug, Args)]
struct FlowCheckArgs {
    #[arg(value_name = "SCRIPT")]
    script: PathBuf,
    #[arg(long, value_parser = parse_opaque_json, allow_hyphen_values = true)]
    args: Option<Value>,
    #[arg(long, value_name = "PATH")]
    catalog: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct AdapterRenderArgs {
    adapter: String,
    #[arg(long, value_name = "PATH")]
    cwd: Option<PathBuf>,
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

#[derive(Debug, Subcommand)]
enum AttestCommand {
    Exec(AttestExecArgs),
}

#[derive(Debug, Args)]
struct AttestExecArgs {
    #[arg(long)]
    task_uuid: String,
    #[arg(long)]
    attempt: u32,
    #[arg(long)]
    lease_epoch: u64,
    #[arg(long)]
    payload_hash: Option<String>,
    #[arg(long)]
    brief_hash: Option<String>,
    #[arg(long)]
    adapter: Option<String>,
    #[arg(long)]
    executor: Option<String>,
    #[arg(long, value_name = "SPEC")]
    evidence: Vec<String>,
    #[arg(long, value_name = "PATH")]
    ledger: Option<PathBuf>,
    #[arg(last = true, required = true)]
    argv: Vec<String>,
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
    #[arg(long)]
    executor: Option<String>,
    #[arg(long, value_enum, default_value = "medium")]
    priority: CliPriority,
    #[arg(long, default_value = "shell")]
    adapter: String,
    #[arg(long, value_name = "PATH")]
    cwd: Option<PathBuf>,
    #[arg(long = "env", value_parser = parse_environment, action = clap::ArgAction::Append)]
    environment: Vec<(String, String)>,
    #[arg(long = "pre-prompt-arg", allow_hyphen_values = true, action = clap::ArgAction::Append)]
    pre_prompt_argv: Vec<String>,
    #[arg(long)]
    approval_policy: Option<String>,
    #[arg(long)]
    sandbox_policy: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    effort: Option<String>,
    #[arg(long)]
    workspace_repo: Option<String>,
    #[arg(long)]
    workspace_base_rev: Option<String>,
    #[arg(long)]
    workspace_branch: Option<String>,
    #[arg(long, value_name = "PATH")]
    workspace_worktree: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    gate_manifest: Option<PathBuf>,
    #[arg(
        long,
        value_parser = parse_opaque_json,
        allow_hyphen_values = true,
        conflicts_with = "brief_path"
    )]
    brief: Option<Value>,
    #[arg(long, value_name = "PATH", conflicts_with = "brief")]
    brief_path: Option<PathBuf>,
    #[arg(long = "required-gate", action = clap::ArgAction::Append)]
    required_gate_ids: Vec<String>,
    #[arg(long, value_enum)]
    acceptance_policy: Option<CliAcceptancePolicy>,
    #[arg(long, value_enum, default_value = "manual")]
    source: CliSource,
    #[arg(long)]
    dedup_key: Option<String>,
    #[arg(long, value_parser = parse_orchestration, allow_hyphen_values = true)]
    orchestration: Option<Orchestration>,
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
    #[arg(long, value_parser = parse_related_trigger, allow_hyphen_values = true)]
    related_trigger: Option<RelatedTrigger>,
    #[arg(long)]
    wait: bool,
    #[arg(last = true)]
    argv: Vec<String>,
}

#[derive(Debug, Args)]
struct GcArgs {
    #[arg(long, value_name = "DURATION")]
    horizon: String,
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    collect: bool,
    #[arg(long, value_name = "PATH")]
    data_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum ViewCommand {
    Rebuild(ViewRebuildArgs),
}

#[derive(Debug, Args)]
struct ViewRebuildArgs {
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    yes: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliAcceptancePolicy {
    Manual,
    ExecutionAndGates,
}

impl From<CliAcceptancePolicy> for AcceptancePolicy {
    fn from(value: CliAcceptancePolicy) -> Self {
        match value {
            CliAcceptancePolicy::Manual => Self::Manual,
            CliAcceptancePolicy::ExecutionAndGates => Self::ExecutionAndGates,
        }
    }
}

fn parse_environment(value: &str) -> Result<(String, String), String> {
    let (name, value) = value
        .split_once('=')
        .ok_or_else(|| "environment must use NAME=VALUE".to_owned())?;
    let mut bytes = name.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || name.starts_with("TALLY_")
        || name == "CREDENTIALS_DIRECTORY"
    {
        return Err(format!("environment name {name:?} is invalid or reserved"));
    }
    if value.contains('\0') {
        return Err("environment value contains a NUL byte".to_owned());
    }
    Ok((name.to_owned(), value.to_owned()))
}

fn parse_opaque_json(value: &str) -> Result<Value, String> {
    serde_json::from_str(value).map_err(|error| format!("invalid JSON value: {error}"))
}

fn parse_orchestration(value: &str) -> Result<Orchestration, String> {
    serde_json::from_str(value).map_err(|error| format!("invalid orchestration capsule: {error}"))
}

fn parse_related_trigger(value: &str) -> Result<RelatedTrigger, String> {
    let related: RelatedTrigger = serde_json::from_str(value)
        .map_err(|error| format!("invalid related trigger JSON: {error}"))?;
    related
        .validate()
        .map_err(|error| format!("invalid related trigger: {error}"))?;
    Ok(related)
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
    Continue {
        job: String,
        #[arg(long)]
        wait: bool,
        #[arg(last = true, required = true)]
        argv: Vec<String>,
    },
    Retry {
        job: String,
    },
    Drain,
    AwaitJob {
        job: String,
    },
    AwaitBarrier {
        barrier: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum GhDiagnosticEvent {
    CommandComment,
    Mention,
    Assignment,
    Label,
}

impl GhDiagnosticEvent {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CommandComment => "command-comment",
            Self::Mention => "mention",
            Self::Assignment => "assignment",
            Self::Label => "label",
        }
    }
}

#[derive(Debug, Subcommand)]
enum ProducerCommand {
    Preview {
        name: String,
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
    },
    Poll {
        name: String,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        no_enqueue: bool,
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
    },
    Explain {
        name: String,
        #[arg(long)]
        item: String,
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
    },
    Test {
        name: String,
        #[arg(long)]
        item: String,
        #[arg(long, value_enum)]
        event: GhDiagnosticEvent,
        #[arg(long)]
        actor: String,
        #[arg(long, conflicts_with = "promote")]
        no_enqueue: bool,
        #[arg(long)]
        promote: bool,
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
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
        #[arg(long = "exec-attestations", value_name = "PATH")]
        exec_attestations: Vec<PathBuf>,
        #[arg(long, value_enum, default_value = "text")]
        format: WitnessVerifyFormat,
    },
    VerifyAuthorship {
        #[arg(long, value_name = "PATH")]
        ledger: Option<PathBuf>,
        #[arg(long, alias = "repo", value_name = "DIR")]
        repository: PathBuf,
        #[arg(long, value_name = "UUID")]
        task: String,
        #[arg(long)]
        attempt: Option<u32>,
        #[arg(long)]
        lease_epoch: Option<u64>,
        #[arg(long, value_enum, default_value = "text")]
        format: WitnessVerifyFormat,
    },
    Compare {
        #[arg(long, value_name = "DIR", conflicts_with = "canon")]
        data_dir: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        canon: Option<PathBuf>,
        #[arg(long, value_name = "PATH", required = true, num_args = 1..)]
        attestations: Vec<PathBuf>,
        #[arg(long, value_enum, default_value = "text")]
        format: WitnessVerifyFormat,
        #[arg(long)]
        strict: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum WitnessVerifyFormat {
    Text,
    Json,
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
    Jobs {
        #[arg(long, alias = "live-state")]
        state: Option<String>,
        #[arg(long, alias = "terminal-verdict")]
        verdict: Option<String>,
        #[arg(long)]
        pool: Option<String>,
        #[arg(long)]
        executor: Option<String>,
        #[arg(long)]
        adapter: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        origin: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        #[arg(long)]
        flow_run: Option<String>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        cursor: Option<String>,
    },
    Job {
        id: String,
    },
    Status {
        #[arg(long)]
        pool: Option<String>,
    },
    Log {
        #[arg(long)]
        task: Option<String>,
        #[arg(long)]
        attempt: Option<u32>,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        event: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long)]
        since: Option<String>,
        #[arg(long)]
        until: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        cursor: Option<String>,
    },
    Proof {
        #[arg(long)]
        task: String,
        #[arg(long)]
        attempt: Option<u32>,
    },
    Trace {
        #[arg(long)]
        task: String,
        #[arg(long)]
        attempt: Option<u32>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        cursor: Option<String>,
    },
    Producers {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        kind: Option<String>,
    },
    Watch {
        #[arg(long)]
        after: Option<String>,
        #[arg(long, hide = true)]
        once: bool,
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
    exit_failure(2, message)
}

fn exit_failure(code: i32, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(ExitFailure {
        code,
        message: message.into(),
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let helper_mode = std::env::args_os().nth(1).is_some_and(|argument| {
        matches!(
            argument.to_str(),
            Some("__record-unit-exit" | "__remote-executor")
        )
    });
    match run().await {
        Ok(()) => {}
        Err(error) => {
            if !helper_mode && !error.to_string().is_empty() {
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

struct JsonlLifecycleSink;

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

async fn run_flow(socket: &Path, config_path: Option<&Path>, command: FlowCommand) -> Result<()> {
    match command {
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
            println!("{}", serde_json::to_string(&checked.meta_json)?);
            Ok(())
        }
        FlowCommand::Run(args) => {
            let source = std::fs::read_to_string(&args.script)
                .with_context(|| format!("cannot read flow script {}", args.script.display()))?;
            let inherited_task_uuid = std::env::var("TALLY_TASK_UUID").ok();
            let inherited_job_id = std::env::var("TALLY_JOB_ID").ok();
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
            let max_frame_bytes = client_config
                .as_ref()
                .map_or(DEFAULT_MAX_FRAME_BYTES, |config| config.max_frame_bytes);
            if let Some(config) = client_config {
                options.adapter_skill_revisions = config
                    .adapters
                    .iter()
                    .filter_map(|(name, adapter)| {
                        adapter
                            .resolved_skill_revision()
                            .map(|revision| (name.clone(), revision))
                    })
                    .collect();
                options.pool_credentials = config
                    .pools
                    .into_iter()
                    .map(|(name, pool)| (name, pool.credentials))
                    .collect();
            }
            sanitize_inherited_tally_environment();
            let socket = socket.to_owned();
            let script = args.script;
            let runtime = tokio::runtime::Handle::current();
            let outcome = tokio::task::spawn_blocking(move || {
                let _runtime = runtime.enter();
                run_script(
                    &source,
                    Some(&script),
                    Rc::new(LiveFlowClient::new(socket, max_frame_bytes, runner)),
                    Rc::new(JsonlLifecycleSink),
                    options,
                )
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

fn captured_runner_identity(
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

fn sanitize_inherited_tally_environment() {
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

fn flow_error(error: FlowError) -> anyhow::Error {
    let code = match error.code.as_str() {
        "replay-divergence" | "script-changed-mid-run" => 20,
        "flow-run-id-missing"
        | "flow-run-id-invalid"
        | "runner-identity-invalid"
        | "runner-identity-incomplete" => 2,
        "script-syntax"
        | "script-encoding"
        | "script-evaluation"
        | "script-exception"
        | "unhandled-rejection"
        | "determinism-violation"
        | "iteration-cap"
        | "runtime-limit" => 10,
        _ => 1,
    };
    let message = serde_json::to_string(&error.report()).unwrap_or_else(|_| error.to_string());
    anyhow::Error::new(ExitFailure { code, message })
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
        Some(Command::RemoteExecutor) => {
            serve_remote_executor_stdio().await?;
            Ok(())
        }
        Some(Command::AdapterRender(args)) => {
            let config_path = opts
                .config
                .context("__adapter-render requires --config PATH")?;
            let config = Config::from_path(&config_path)?;
            let engine = AdapterEngine::new(&config.adapters);
            let options = AdapterJobOptions::default();
            let (invocation, scraped) = if let Some(captures) = args.captures {
                let captures = ScrapeResult {
                    captures: serde_json::from_str(&captures)
                        .context("--captures must be a JSON object")?,
                };
                (
                    engine.resume_with_options(
                        &args.adapter,
                        &args.argv,
                        &captures,
                        &options,
                        args.cwd.as_deref(),
                    )?,
                    None,
                )
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
                let invocation = engine.resume_with_options(
                    &args.adapter,
                    &args.argv,
                    &captures,
                    &options,
                    args.cwd.as_deref(),
                )?;
                (invocation, Some(captures.captures))
            } else {
                (
                    engine.launch_with_options(
                        &args.adapter,
                        &args.argv,
                        &options,
                        args.cwd.as_deref(),
                    )?,
                    None,
                )
            };
            println!(
                "{}",
                serde_json::to_string(&json!({
                    "argv": invocation.argv,
                    "env": invocation.env,
                    "hardening": invocation.hardening,
                    "yieldHook": invocation.yield_hook,
                    "captures": scraped,
                    "defaultGateManifest": provisions_gate_manifest(&args.adapter),
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
        }) => {
            print_rpc(
                &socket,
                opts.config.as_deref(),
                "queue.drain",
                Some(json!({})),
            )
            .await
        }
        Some(Command::Enqueue(args)) => run_enqueue(&socket, opts.config.as_deref(), *args).await,
        Some(Command::Gc(args)) => run_gc(args),
        Some(Command::Queue {
            command: QueueCommand::Enqueue(args),
        }) => run_enqueue(&socket, opts.config.as_deref(), *args).await,
        Some(Command::Queue { command }) => {
            run_queue(&socket, opts.config.as_deref(), command).await
        }
        Some(Command::Producer { command }) => run_producer(opts.config, command),
        Some(Command::Witness { command }) => run_witness(command),
        Some(Command::View { command }) => run_view(command).await,
        Some(Command::Attest { command }) => run_attest(command),
        Some(Command::Lease { command }) => {
            run_lease(&socket, opts.config.as_deref(), command).await
        }
        Some(Command::Query { command }) => {
            run_query(&socket, opts.config.as_deref(), command).await
        }
        Some(Command::Flow { command }) => run_flow(&socket, opts.config.as_deref(), command).await,
        None => {
            Opts::command().print_help()?;
            println!();
            Ok(())
        }
    }
}

fn run_producer(config_path: Option<PathBuf>, command: ProducerCommand) -> Result<()> {
    let config_path = config_path.map_or_else(default_config_path, Ok)?;
    let config = Config::from_path(&config_path)?;
    let intake = GhCliIntake::default();
    let now = Utc::now();
    let result = match command {
        ProducerCommand::Preview { name, state_dir } => {
            let state_dir = state_dir.map_or_else(default_state_dir, Ok)?;
            let engine =
                ProducerEngine::new(&config.producers, state_dir.join("events"), &state_dir);
            serde_json::to_value(engine.preview_gh(&name, &intake, now)?)?
        }
        ProducerCommand::Poll {
            name,
            once,
            no_enqueue,
            state_dir,
        } => {
            if !once {
                return Err(invalid("producer poll currently requires --once"));
            }
            let state_dir = state_dir.map_or_else(default_state_dir, Ok)?;
            let engine =
                ProducerEngine::new(&config.producers, state_dir.join("events"), &state_dir);
            if no_enqueue {
                serde_json::to_value(engine.preview_gh(&name, &intake, now)?)?
            } else {
                let mut acknowledgements = GhCliAcknowledgementSink::default();
                serde_json::to_value(engine.poll_gh_with_acknowledgements(
                    &name,
                    &intake,
                    now,
                    &mut acknowledgements,
                )?)?
            }
        }
        ProducerCommand::Explain {
            name,
            item,
            state_dir,
        } => {
            let state_dir = state_dir.map_or_else(default_state_dir, Ok)?;
            let engine =
                ProducerEngine::new(&config.producers, state_dir.join("events"), &state_dir);
            serde_json::to_value(engine.explain_gh(&name, &intake, &item, now)?)?
        }
        ProducerCommand::Test {
            name,
            item,
            event,
            actor,
            no_enqueue,
            promote,
            state_dir,
        } => {
            let dry_run = no_enqueue || !promote;
            let state_dir = if promote {
                state_dir.map_or_else(default_state_dir, Ok)?
            } else {
                state_dir.unwrap_or_else(|| {
                    std::env::temp_dir().join(format!("tally-producer-test-{}", std::process::id()))
                })
            };
            let engine =
                ProducerEngine::new(&config.producers, state_dir.join("events"), &state_dir);
            let observation = engine.diagnostic_gh_observation(
                &name,
                &intake,
                &item,
                event.as_str(),
                &actor,
                now,
            )?;
            if dry_run {
                serde_json::to_value(engine.preview_gh_observation(&name, &observation, now)?)?
            } else {
                let mut acknowledgements = GhCliAcknowledgementSink::default();
                serde_json::to_value(engine.admit_gh_observation(
                    &name,
                    &observation,
                    now,
                    &mut acknowledgements,
                )?)?
            }
        }
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

async fn run_producer_dispatch(
    config_path: Option<PathBuf>,
    socket: &Path,
    args: ProducerDispatchArgs,
) -> Result<()> {
    let config_path = config_path.context("__producer-dispatch requires --config PATH")?;
    let config = Config::from_path(&config_path)?;
    let max_frame_bytes = config.max_frame_bytes;
    let event: ProducerObservation = serde_json::from_str(&args.event)
        .context("--event must be a producer observation JSON object")?;
    let state_dir = args.state_dir.map_or_else(default_state_dir, Ok)?;
    let events_dir = state_dir.join("events");
    let engine = ProducerEngine::new(&config.producers, &events_dir, &state_dir);
    let expected_kind = match &event {
        ProducerObservation::Calendar => "calendar",
        ProducerObservation::EventsDir => "events-dir",
        ProducerObservation::Gh(_) => "gh",
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
    let producer_name = args.producer.clone();
    let dispatched: Result<Value> = async {
        let result = match event {
        ProducerObservation::Calendar => {
            serde_json::to_value(engine.emit_calendar(&args.producer, now)?)?
        }
        ProducerObservation::EventsDir => {
            let client =
                RpcClient::connect_with_max_frame_bytes(socket, max_frame_bytes).await?;
            client
                .call(
                    "queue.drain",
                    Some(json!({"producer": args.producer.clone()})),
                )
                .await?
        }
        ProducerObservation::Gh(observation) => {
            let tally_core::producers::GhObservationInput {
                source,
                repo,
                number,
                html_url,
                item_type,
                head_sha,
                node_id,
                item_author,
                trigger_actor,
                self_actor,
                notification_reason,
                trigger_kind,
                event_id,
                comment_id,
                trigger_timestamp,
                trigger_value,
                context,
            } = *observation;
            let is_poll = source.is_none()
                && repo.is_none()
                && number.is_none()
                && html_url.is_none()
                && item_type.is_none()
                && head_sha.is_none()
                && node_id.is_none()
                && item_author.is_none()
                && trigger_actor.is_none()
                && self_actor.is_none()
                && notification_reason.is_none()
                && trigger_kind.is_none()
                && event_id.is_none()
                && comment_id.is_none()
                && trigger_timestamp.is_none()
                && trigger_value.is_none()
                && context.is_none();
            if is_poll {
                let mut acknowledgements = GhCliAcknowledgementSink::default();
                serde_json::to_value(engine.poll_gh_with_acknowledgements(
                    &args.producer,
                    &GhCliIntake::default(),
                    now,
                    &mut acknowledgements,
                )?)?
            } else if let (
                Some(source),
                Some(repo),
                Some(number),
                Some(html_url),
                Some(item_type),
                Some(node_id),
                Some(item_author),
                Some(trigger_actor),
                Some(self_actor),
                Some(trigger_kind),
                Some(trigger_timestamp),
                Some(context),
            ) = (
                source,
                repo,
                number,
                html_url,
                item_type,
                node_id,
                item_author,
                trigger_actor,
                self_actor,
                trigger_kind,
                trigger_timestamp,
                context,
            ) {
                serde_json::to_value(engine.emit_gh(
                    &args.producer,
                    &GhObservation {
                        source,
                        repo,
                        number,
                        html_url,
                        item_type,
                        head_sha,
                        node_id,
                        item_author,
                        trigger_actor,
                        self_actor,
                        notification_reason,
                        trigger_kind,
                        event_id,
                        comment_id,
                        trigger_timestamp,
                        trigger_value,
                        context,
                    },
                    now,
                )?)?
            } else {
                bail!(
                    "gh observation requires either no fields for a configured poll or complete origin identity and context fields"
                )
            }
        }
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
                    let client =
                        RpcClient::connect_with_max_frame_bytes(socket, max_frame_bytes).await?;
                    client
                        .call(
                            "__producer.pool-transition",
                            Some(json!({
                                "producer": args.producer.clone(),
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
        Ok(result)
    }
    .await;
    let runtime = match &dispatched {
        Ok(result) => {
            record_producer_runtime(&state_dir, &producer_name, now, Some(result.clone()), None)
        }
        Err(error) => record_producer_runtime(
            &state_dir,
            &producer_name,
            now,
            None,
            Some(format!("{error:#}")),
        ),
    };
    let runtime_recorded = runtime.is_ok();
    if let Err(error) = runtime {
        eprintln!(
            "tally: producer runtime state for {producer_name:?} could not be recorded: {error}"
        );
    }
    if runtime_recorded && !args.engine_only {
        match RpcClient::connect_with_max_frame_bytes(socket, max_frame_bytes).await {
            Ok(client) => {
                if let Err(error) = client
                    .call(
                        "__producer.runtime-observed",
                        Some(json!({"producer": producer_name.clone()})),
                    )
                    .await
                {
                    eprintln!(
                        "tally: producer runtime update for {producer_name:?} could not notify the daemon: {error}"
                    );
                }
            }
            Err(error) => {
                eprintln!(
                    "tally: producer runtime update for {producer_name:?} could not reach the daemon: {error}"
                );
            }
        }
    }
    let result = dispatched?;
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

async fn run_enqueue(
    socket: &Path,
    config_path: Option<&Path>,
    mut args: EnqueueArgs,
) -> Result<()> {
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
    let workspace = match (
        args.workspace_repo,
        args.workspace_base_rev,
        args.workspace_branch,
        args.workspace_worktree,
    ) {
        (None, None, None, None) => None,
        (Some(repo), Some(base_rev), Some(branch), Some(worktree_path)) => {
            Some(WorkspaceMetadata {
                repo,
                base_rev,
                branch,
                worktree_path,
            })
        }
        _ => {
            return Err(invalid(
                "workspace metadata requires --workspace-repo, --workspace-base-rev, --workspace-branch, and --workspace-worktree together",
            ))
        }
    };
    let cwd = args.cwd.or_else(|| {
        workspace
            .as_ref()
            .map(|workspace| workspace.worktree_path.clone())
    });
    let gate_manifest = match (
        args.gate_manifest,
        args.required_gate_ids.is_empty(),
        args.acceptance_policy,
    ) {
        (None, true, None) => None,
        (Some(path), false, policy) => Some(GateManifestSpec {
            path,
            required_gate_ids: args.required_gate_ids,
            acceptance_policy: policy
                .map(Into::into)
                .unwrap_or(AcceptancePolicy::Manual),
        }),
        _ => {
            return Err(invalid(
                "--gate-manifest requires at least one --required-gate; --required-gate and --acceptance-policy require --gate-manifest",
            ))
        }
    };
    let mut environment = BTreeMap::new();
    for (name, value) in args.environment {
        if environment.insert(name.clone(), value).is_some() {
            return Err(invalid(format!(
                "environment variable {name:?} is repeated"
            )));
        }
    }
    let adapter_options = AdapterJobOptions {
        pre_prompt_argv: args.pre_prompt_argv,
        environment,
        approval_policy: args.approval_policy,
        sandbox_policy: args.sandbox_policy,
        model: args.model,
        effort: args.effort,
    };
    let payload = EnqueuePayload {
        invocation: args.invocation,
        argv: has_argv.then_some(args.argv),
        pools: Some(args.pools),
        executor: args.executor,
        priority: Some(args.priority.into()),
        adapter: Some(args.adapter),
        cwd,
        workspace,
        adapter_options: (!adapter_options.is_default()).then_some(adapter_options),
        gate_manifest,
        brief: args.brief,
        brief_path: args.brief_path,
        resume_from: None,
        source: Some(args.source.into()),
        dedup_key: args.dedup_key,
        submission: None,
        orchestration: args.orchestration,
        parent: args.parent,
        evidence: args.evidence,
        drv: None,
        evidence_class: args.evidence_class,
        manifest_hash: args.manifest_hash,
        consumption_estimate: args.consumption_estimate,
        runtime_max_sec: args.runtime_max_sec,
        no_enqueue: args.no_enqueue,
        credentials: Default::default(),
        origin: None,
        caller_job_id: std::env::var("TALLY_JOB_ID").ok(),
        gh_trigger_actor: None,
        gh_self_actor: None,
        gh_origin: None,
        task_uuid: None,
        related_trigger: args.related_trigger,
        wait: args.wait,
    };
    submit_payload(socket, config_path, "queue.enqueue", payload, args.wait).await
}

async fn submit_payload(
    socket: &Path,
    config_path: Option<&Path>,
    method: &str,
    payload: EnqueuePayload,
    wait: bool,
) -> Result<()> {
    let client = connect_rpc(socket, config_path).await?;
    let result = client
        .call(method, Some(serde_json::to_value(payload)?))
        .await?;
    if !wait {
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

async fn run_queue(socket: &Path, config_path: Option<&Path>, command: QueueCommand) -> Result<()> {
    match command {
        QueueCommand::Enqueue(_) => unreachable!("enqueue is routed before run_queue"),
        QueueCommand::Cancel { job, force } => {
            print_rpc(
                socket,
                config_path,
                "queue.cancel",
                Some(json!({"task_uuid": job, "force": force})),
            )
            .await
        }
        QueueCommand::Pause { pool, all } => {
            print_rpc(
                socket,
                config_path,
                "queue.pause",
                Some(json!({"pool": pool, "all": all})),
            )
            .await
        }
        QueueCommand::Resume { pool, all } => {
            print_rpc(
                socket,
                config_path,
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
                caller_job_id: None,
                gh_trigger_actor: None,
                gh_self_actor: None,
                gh_origin: None,
                task_uuid: None,
                related_trigger: None,
                wait,
            };
            submit_payload(socket, config_path, "queue.continue", payload, wait).await
        }
        QueueCommand::Retry { job } => {
            print_rpc(
                socket,
                config_path,
                "queue.retry",
                Some(json!({"task_uuid": job})),
            )
            .await
        }
        QueueCommand::Drain => print_rpc(socket, config_path, "queue.drain", Some(json!({}))).await,
        QueueCommand::AwaitJob { job } => {
            print_rpc(
                socket,
                config_path,
                "queue.await_job",
                Some(json!({"task_uuid": job})),
            )
            .await
        }
        QueueCommand::AwaitBarrier { barrier } => {
            print_rpc(
                socket,
                config_path,
                "queue.await_barrier",
                Some(json!({"barrier": barrier})),
            )
            .await
        }
    }
}

async fn run_lease(socket: &Path, config_path: Option<&Path>, command: LeaseCommand) -> Result<()> {
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
                "lease.acquire",
                Some(json!({"pool": pool})),
            )
            .await
        }
        LeaseCommand::Release { lease } => {
            print_rpc(
                socket,
                config_path,
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
            print_rpc(socket, config_path, "lease.status", Some(params)).await
        }
    }
}

async fn run_query(socket: &Path, config_path: Option<&Path>, command: QueryCommand) -> Result<()> {
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
            print_rpc(socket, config_path, "query.job", Some(json!({"id": id}))).await
        }
        QueryCommand::Status { pool } => {
            print_rpc(
                socket,
                config_path,
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
                "query.standup",
                Some(json!({"since": since})),
            )
            .await
        }
        QueryCommand::Pools => print_rpc(socket, config_path, "query.pools", Some(json!({}))).await,
    }
}

async fn print_rpc(
    socket: &Path,
    config_path: Option<&Path>,
    method: &str,
    params: Option<Value>,
) -> Result<()> {
    let client = connect_rpc(socket, config_path).await?;
    let result = client.call(method, params).await?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

async fn connect_rpc(socket: &Path, config_path: Option<&Path>) -> Result<RpcClient> {
    let max_frame_bytes = client_max_frame_bytes(config_path)?;
    RpcClient::connect_with_max_frame_bytes(socket, max_frame_bytes)
        .await
        .map_err(Into::into)
}

fn client_max_frame_bytes(config_path: Option<&Path>) -> Result<u64> {
    resolve_max_frame_bytes(config_path).map_err(Into::into)
}

fn load_client_config(config_path: Option<&Path>) -> Result<Option<Config>> {
    let (path, explicit) = if let Some(path) = config_path {
        (path.to_owned(), true)
    } else {
        let Ok(path) = default_config_path() else {
            return Ok(None);
        };
        (path, false)
    };
    match Config::from_path(&path) {
        Ok(config) => Ok(Some(config)),
        Err(tally_core::ConfigError::Read { source, .. })
            if !explicit && source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

async fn run_query_watch(
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
            exec_attestations,
            format,
        } => {
            let ledger = path
                .or(ledger)
                .unwrap_or(default_data_dir()?.join("witness.jsonl"));
            let attestations =
                attestations.unwrap_or_else(|| ledger.with_file_name("attestations.jsonl"));
            let (verdict_report, verdict_records) = read_verified_records(&ledger)?;
            let (attestation_report, attestation_records) =
                read_verified_attestations(&attestations)?;
            let mut exec_reports = Vec::with_capacity(exec_attestations.len());
            let mut exec_ok = true;
            for path in exec_attestations {
                let (report, _) = read_verified_exec_attestations(&path)?;
                exec_ok &= report.ok;
                exec_reports.push((path, report));
            }
            match format {
                WitnessVerifyFormat::Text => {
                    if verdict_report.ok {
                        println!(
                            "verdict chain: ok ({} records, seq {:?}..{:?})",
                            verdict_report.records,
                            verdict_report.first_seq,
                            verdict_report.last_seq
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
                    for (path, report) in &exec_reports {
                        println!(
                            "execution attestation chain {}: {} ({} records; {})",
                            path.display(),
                            if report.ok { "ok" } else { "invalid" },
                            report.records,
                            report.authentication
                        );
                        if let Some(problem) = &report.problem {
                            println!("  {problem}");
                        }
                    }
                }
                WitnessVerifyFormat::Json => {
                    let verdict_head = verdict_records.last().map_or_else(
                        || json!({"seq": 0, "hash": GENESIS_PREV_HASH}),
                        |record| json!({"seq": record.seq, "hash": record.hash}),
                    );
                    let attestation_head = attestation_records.last().map_or_else(
                        || json!({"seq": 0, "hash": GENESIS_PREV_HASH}),
                        |record| json!({"seq": record.seq, "hash": record.hash}),
                    );
                    println!(
                        "{}",
                        serde_json::to_string(&json!({
                            "schemaVersion": 2,
                            "protocolVersion": tally_core::query::QUERY_PROTOCOL_VERSION,
                            "ok": verdict_report.ok && attestation_report.ok && exec_ok,
                            "chains": {
                                "verdict": {
                                    "path": ledger,
                                    "report": verdict_report,
                                    "chainHead": verdict_head,
                                },
                                "attestations": {
                                    "path": attestations,
                                    "report": attestation_report,
                                    "chainHead": attestation_head,
                                },
                            },
                            "execAttestations": exec_reports
                                .iter()
                                .map(|(path, report)| json!({
                                    "path": path,
                                    "report": report,
                                }))
                                .collect::<Vec<_>>(),
                        }))?
                    );
                }
            }
            if !verdict_report.ok || !attestation_report.ok || !exec_ok {
                bail!("ledger verification failed");
            }
            Ok(())
        }
        WitnessCommand::Compare {
            data_dir,
            canon,
            attestations,
            format,
            strict,
        } => {
            let canon = match (data_dir, canon) {
                (Some(data_dir), None) => data_dir.join("witness.jsonl"),
                (None, Some(canon)) => canon,
                (None, None) => default_data_dir()?.join("witness.jsonl"),
                (Some(_), Some(_)) => unreachable!("clap rejects conflicting canon selectors"),
            };
            let report = compare_witness_attestations(&canon, &attestations, strict)
                .map_err(|error| exit_failure(2, error.to_string()))?;
            match format {
                WitnessVerifyFormat::Json => {
                    println!("{}", serde_json::to_string(&report)?);
                }
                WitnessVerifyFormat::Text => {
                    for execution in &report.executions {
                        println!(
                            "{} {} {:?}",
                            execution.witness_ref, execution.execution_id, execution.agreement
                        );
                        for diff in &execution.diffs {
                            println!("  {diff}");
                        }
                    }
                    println!(
                        "compared={} unanimous={} diverged={} unattested={} orphans={}",
                        report.summary.compared,
                        report.summary.unanimous,
                        report.summary.diverged,
                        report.summary.unattested,
                        report.summary.orphans
                    );
                }
            }
            if report.ok {
                Ok(())
            } else {
                Err(exit_failure(1, String::new()))
            }
        }
        WitnessCommand::VerifyAuthorship {
            ledger,
            repository,
            task,
            attempt,
            lease_epoch,
            format,
        } => {
            let ledger = ledger.unwrap_or(default_data_dir()?.join("witness.jsonl"));
            let report =
                verify_authorship(&ledger, &repository, task.as_str(), attempt, lease_epoch)?;
            match format {
                WitnessVerifyFormat::Json => {
                    println!("{}", serde_json::to_string(&report)?);
                }
                WitnessVerifyFormat::Text => {
                    println!(
                        "authorship binding: {}",
                        serde_json::to_value(report.status)?
                            .as_str()
                            .expect("status serializes as a string")
                    );
                    println!(
                        "verdict chain: {} ({} records)",
                        if report.ledger.ok { "ok" } else { "invalid" },
                        report.ledger.records
                    );
                    if let Some(revision) = &report.result_revision {
                        println!("result revision: {revision}");
                    }
                    if let Some(expected) = &report.expected_note_content_sha256 {
                        println!("expected note digest: {expected}");
                    }
                    if let Some(observed) = &report.observed_note_content_sha256 {
                        println!("observed note digest: {observed}");
                    }
                    if let Some(expected) = &report.expected_notes_ref_target {
                        println!("expected notes-ref target: {expected}");
                    }
                    if let Some(observed) = &report.observed_notes_ref_target {
                        println!("observed notes-ref target: {observed}");
                    }
                    if let Some(reason) = &report.reason {
                        println!("reason: {reason}");
                    }
                }
            }
            if report.ok {
                Ok(())
            } else {
                Err(exit_failure(1, String::new()))
            }
        }
    }
}

fn run_attest(command: AttestCommand) -> Result<()> {
    match command {
        AttestCommand::Exec(args) => {
            let ledger = args
                .ledger
                .unwrap_or(default_state_dir()?.join(EXEC_ATTESTATION_LEDGER));
            let outcome = run_exec(ExecRunRequest {
                ledger,
                task_uuid: args.task_uuid,
                attempt: args.attempt,
                lease_epoch: args.lease_epoch,
                payload_hash: args.payload_hash,
                brief_hash: args.brief_hash,
                adapter: args.adapter,
                executor: args.executor,
                evidence: args.evidence,
                argv: args.argv,
            })
            .map_err(|error| invalid(error.to_string()))?;
            if outcome.exit_code == 0 {
                Ok(())
            } else {
                Err(exit_failure(outcome.exit_code, String::new()))
            }
        }
    }
}

fn run_gc(args: GcArgs) -> Result<()> {
    let data_dir = args.data_dir.map_or_else(default_data_dir, Ok)?;
    if !data_dir.is_absolute() {
        return Err(invalid("--data-dir must be absolute"));
    }
    let report = tally_core::retention::run_gc(
        &data_dir,
        &args.horizon,
        Utc::now(),
        args.dry_run,
        args.collect,
        &tally_core::nix_store::NixStore::default(),
    )?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

async fn run_view(command: ViewCommand) -> Result<()> {
    match command {
        ViewCommand::Rebuild(args) => {
            let state_dir = default_state_dir()?;
            let data_dir = args.data_dir.map_or_else(default_data_dir, Ok)?;
            if !state_dir.is_absolute() {
                return Err(invalid("the tally state directory must be absolute"));
            }
            if !data_dir.is_absolute() {
                return Err(invalid("--data-dir must be absolute"));
            }

            let taskdata_dir = data_dir.join(TASKDATA_DIRECTORY);
            if !args.yes && std::fs::symlink_metadata(&taskdata_dir).is_ok() {
                eprint!(
                    "tally: archive {} and rebuild the TaskChampion view from durable facts? [y/N] ",
                    taskdata_dir.display()
                );
                std::io::stderr().flush()?;
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    return Err(invalid(
                        "view rebuild cancelled; pass --yes to confirm non-interactively",
                    ));
                }
            }

            let report =
                tally_core::view::rebuild_taskchampion_view(&state_dir, &data_dir, Utc::now())
                    .await?;
            println!("{}", serde_json::to_string(&report)?);
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
        "pass" | "reused" | "substituted" => 0,
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
    fn authorship_verifier_cli_selects_an_exact_witness_lane() {
        let options = Opts::try_parse_from([
            "tally",
            "witness",
            "verify-authorship",
            "--ledger",
            "/tmp/witness.jsonl",
            "--repository",
            "/tmp/repository",
            "--task",
            "00000000-0000-4000-8000-000000000053",
            "--attempt",
            "2",
            "--lease-epoch",
            "7",
            "--format",
            "json",
        ])
        .unwrap();
        assert!(matches!(
            options.command,
            Some(Command::Witness {
                command: WitnessCommand::VerifyAuthorship {
                    ledger: Some(ledger),
                    repository,
                    task,
                    attempt: Some(2),
                    lease_epoch: Some(7),
                    format: WitnessVerifyFormat::Json,
                }
            }) if ledger == Path::new("/tmp/witness.jsonl")
                && repository == Path::new("/tmp/repository")
                && task == "00000000-0000-4000-8000-000000000053"
        ));
    }

    #[test]
    fn explicit_client_config_controls_the_transport_limit() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("config.json");
        std::fs::write(
            &config,
            concat!(
                r#"{"maxFrameBytes":20971520,"agingThresholdSec":3600,"pools":{"slot":{"#,
                r#""credentials":{"token":"/run/credentials/slot-token"}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(
            client_max_frame_bytes(Some(&config)).unwrap(),
            20 * 1024 * 1024
        );
        assert_eq!(
            load_client_config(Some(&config)).unwrap().unwrap().pools["slot"].credentials["token"],
            PathBuf::from("/run/credentials/slot-token")
        );
        assert!(client_max_frame_bytes(Some(&temp.path().join("missing.json"))).is_err());
    }

    #[test]
    fn runner_identity_is_all_or_nothing_and_uuid_typed() {
        assert_eq!(
            captured_runner_identity(None, None).unwrap(),
            RunnerIdentity::default()
        );
        let identity = captured_runner_identity(
            Some("00000000-0000-4000-8000-000000000071".to_owned()),
            Some("00000000-0000-4000-8000-000000000072".to_owned()),
        )
        .unwrap();
        assert_eq!(
            identity.task_uuid.as_deref(),
            Some("00000000-0000-4000-8000-000000000071")
        );
        assert_eq!(
            captured_runner_identity(
                Some("00000000-0000-4000-8000-000000000071".to_owned()),
                None
            )
            .unwrap_err()
            .code,
            "runner-identity-incomplete"
        );
        assert_eq!(
            captured_runner_identity(Some("not-a-uuid".to_owned()), Some("also-bad".to_owned()))
                .unwrap_err()
                .code,
            "runner-identity-invalid"
        );
    }

    #[test]
    fn flow_failure_taxonomy_has_distinguished_exit_codes() {
        let exit_code = |code| {
            error_exit_code(&flow_error(FlowError::new(
                "FlowTestError",
                code,
                "fixture",
            )))
        };
        assert_eq!(exit_code("script-syntax"), 10);
        assert_eq!(exit_code("script-evaluation"), 10);
        assert_eq!(exit_code("determinism-violation"), 10);
        assert_eq!(exit_code("replay-divergence"), 20);
        assert_eq!(exit_code("script-changed-mid-run"), 20);
        assert_eq!(exit_code("flow-run-id-missing"), 2);
        assert_eq!(exit_code("runner-identity-incomplete"), 2);
        assert_eq!(exit_code("terminal-failure"), 1);
    }

    #[test]
    fn full_top_level_surface_is_visible() {
        let help = Opts::command().render_long_help().to_string();
        for verb in [
            "enqueue", "queue", "producer", "witness", "lease", "daemon", "query", "flow",
        ] {
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
    fn flow_run_and_check_cli_shapes_match_the_declarative_contract() {
        let check = Opts::try_parse_from([
            "tally",
            "flow",
            "check",
            "/nix/store/example-flow.js",
            "--args",
            r#"{"task":"ship"}"#,
            "--catalog",
            "/nix/store/catalog.json",
        ])
        .unwrap();
        assert!(matches!(
            check.command,
            Some(Command::Flow {
                command: FlowCommand::Check(FlowCheckArgs {
                    script,
                    args: Some(args),
                    catalog: Some(catalog),
                })
            }) if script == Path::new("/nix/store/example-flow.js")
                && args == json!({"task": "ship"})
                && catalog == Path::new("/nix/store/catalog.json")
        ));

        let run = Opts::try_parse_from([
            "tally",
            "flow",
            "run",
            "/nix/store/example-flow.js",
            "--args",
            r#"{"task":"ship"}"#,
            "--max-nodes",
            "200",
            "--flow-run-id",
            "run-47",
        ])
        .unwrap();
        assert!(matches!(
            run.command,
            Some(Command::Flow {
                command: FlowCommand::Run(FlowRunArgs {
                    flow_run_id: Some(flow_run_id),
                    max_nodes: 200,
                    ..
                })
            }) if flow_run_id == "run-47"
        ));
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

    #[test]
    fn enqueue_wave_three_options_and_public_continuation_parse_directly() {
        let options = Opts::try_parse_from([
            "tally",
            "enqueue",
            "--pool",
            "build",
            "--adapter",
            "codex",
            "--cwd",
            "/worktrees/tally",
            "--env",
            "NO_COLOR=1",
            "--pre-prompt-arg",
            "--dangerously-bypass-approvals-and-sandbox",
            "--approval-policy",
            "never",
            "--sandbox-policy",
            "danger-full-access",
            "--model",
            "gpt-5-codex",
            "--effort",
            "high",
            "--workspace-repo",
            "mecattaf/tally.nix",
            "--workspace-base-rev",
            "origin/main",
            "--workspace-branch",
            "wave-3-ergonomics",
            "--workspace-worktree",
            "/worktrees/tally",
            "--gate-manifest",
            "/worktrees/tally/.tally/gates.json",
            "--required-gate",
            "tests",
            "--acceptance-policy",
            "execution-and-gates",
            "--",
            "implement issue 28",
        ])
        .unwrap();
        let Some(Command::Enqueue(args)) = options.command else {
            panic!("expected enqueue command");
        };
        assert_eq!(
            args.pre_prompt_argv,
            ["--dangerously-bypass-approvals-and-sandbox"]
        );
        assert_eq!(args.environment, [("NO_COLOR".to_owned(), "1".to_owned())]);
        assert_eq!(args.workspace_repo.as_deref(), Some("mecattaf/tally.nix"));
        assert_eq!(args.required_gate_ids, ["tests"]);

        let continuation = Opts::try_parse_from([
            "tally",
            "queue",
            "continue",
            "00000000-0000-4000-8000-000000000028",
            "--wait",
            "--",
            "address review",
        ])
        .unwrap();
        assert!(matches!(
            continuation.command,
            Some(Command::Queue {
                command: QueueCommand::Continue {
                    job,
                    wait: true,
                    argv,
                }
            }) if job == "00000000-0000-4000-8000-000000000028"
                && argv == ["address review"]
        ));

        let retry = Opts::try_parse_from([
            "tally",
            "queue",
            "retry",
            "00000000-0000-4000-8000-000000000028",
        ])
        .unwrap();
        assert!(matches!(
            retry.command,
            Some(Command::Queue {
                command: QueueCommand::Retry { job }
            }) if job == "00000000-0000-4000-8000-000000000028"
        ));
    }

    #[test]
    fn producer_diagnostics_and_related_trigger_fallback_parse_strictly() {
        let test = Opts::try_parse_from([
            "tally",
            "producer",
            "test",
            "github",
            "--item",
            "https://github.com/acme/widgets/issues/42",
            "--event",
            "command-comment",
            "--actor",
            "maintainer",
            "--no-enqueue",
        ])
        .unwrap();
        assert!(matches!(
            test.command,
            Some(Command::Producer {
                command: ProducerCommand::Test {
                    name,
                    event: GhDiagnosticEvent::CommandComment,
                    no_enqueue: true,
                    promote: false,
                    ..
                }
            }) if name == "github"
        ));

        let fallback = Opts::try_parse_from([
            "tally",
            "enqueue",
            "--pool",
            "gpu",
            "--source",
            "orchestrator",
            "--related-trigger",
            r#"{"producer":"github","eventId":"comment-42","outcome":"filtered","receiptId":"receipt-42"}"#,
            "--",
            "true",
        ])
        .unwrap();
        let Some(Command::Enqueue(args)) = fallback.command else {
            panic!("expected enqueue command");
        };
        let related = args.related_trigger.unwrap();
        assert_eq!(related.producer, "github");
        assert_eq!(related.event_id, "comment-42");
        assert_eq!(related.receipt_id.as_deref(), Some("receipt-42"));
    }
}
