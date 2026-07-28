use super::*;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum Mode {
    Daemon,
    CheckConfig,
}

#[derive(Debug, Parser)]
#[command(
    name = "tally",
    version,
    about = "Contention and proof for impure labor"
)]
pub(super) struct Opts {
    #[arg(long, value_enum)]
    pub(super) mode: Option<Mode>,
    #[arg(long, global = true, value_name = "PATH")]
    pub(super) config: Option<PathBuf>,
    #[arg(long, global = true, value_name = "PATH")]
    pub(super) socket: Option<PathBuf>,
    #[arg(long, global = true, value_name = "SECONDS")]
    pub(super) rpc_timeout_sec: Option<u64>,
    #[command(subcommand)]
    pub(super) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(super) enum Command {
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
pub(super) enum FlowCommand {
    Run(FlowRunArgs),
    Check(FlowCheckArgs),
    Cancel(FlowCancelArgs),
}

#[derive(Debug, Args)]
pub(super) struct FlowCancelArgs {
    #[arg(value_name = "FLOW_RUN_ID")]
    pub(super) flow_run_id: String,
}

#[derive(Debug, Args)]
pub(super) struct FlowRunArgs {
    #[arg(value_name = "SCRIPT")]
    pub(super) script: PathBuf,
    #[arg(long, default_value = "{}", value_parser = parse_opaque_json, allow_hyphen_values = true)]
    pub(super) args: Value,
    #[arg(long, value_name = "PATH")]
    pub(super) catalog: Option<PathBuf>,
    // `tally flow run` named this `--flow-run-id` while `tally query` named the
    // same value `--flow-run`. Both spellings now work on both sides.
    #[arg(long, alias = "flow-run")]
    pub(super) flow_run_id: Option<String>,
    #[arg(long, default_value_t = tally_flow::DEFAULT_MAX_NODES)]
    pub(super) max_nodes: u32,
    #[arg(long, value_name = "SECONDS")]
    pub(super) rpc_call_deadline_sec: Option<u64>,
}

#[derive(Debug, Args)]
pub(super) struct FlowCheckArgs {
    #[arg(value_name = "SCRIPT")]
    pub(super) script: PathBuf,
    #[arg(long, value_parser = parse_opaque_json, allow_hyphen_values = true)]
    pub(super) args: Option<Value>,
    #[arg(long, value_name = "PATH")]
    pub(super) catalog: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct AdapterRenderArgs {
    pub(super) adapter: String,
    #[arg(long, value_name = "PATH")]
    pub(super) cwd: Option<PathBuf>,
    #[arg(long)]
    pub(super) captures: Option<String>,
    #[arg(
        long,
        value_name = "PATH",
        requires = "scrape_stderr",
        conflicts_with = "captures"
    )]
    pub(super) scrape_stdout: Option<PathBuf>,
    #[arg(
        long,
        value_name = "PATH",
        requires = "scrape_stdout",
        conflicts_with = "captures"
    )]
    pub(super) scrape_stderr: Option<PathBuf>,
    #[arg(last = true)]
    pub(super) argv: Vec<String>,
}

#[derive(Debug, Args)]
pub(super) struct ProducerDispatchArgs {
    pub(super) producer: String,
    #[arg(long)]
    pub(super) event: String,
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub(super) engine_only: bool,
}

#[derive(Debug, Args)]
pub(super) struct RecordUnitExitArgs {
    #[arg(long, value_name = "PATH")]
    pub(super) record: PathBuf,
    #[arg(long)]
    pub(super) unit: String,
}

#[derive(Debug, Subcommand)]
pub(super) enum AttestCommand {
    Exec(AttestExecArgs),
}

#[derive(Debug, Args)]
pub(super) struct AttestExecArgs {
    #[arg(long)]
    pub(super) task_uuid: String,
    #[arg(long)]
    pub(super) attempt: u32,
    #[arg(long)]
    pub(super) lease_epoch: u64,
    #[arg(long)]
    pub(super) payload_hash: Option<String>,
    #[arg(long)]
    pub(super) brief_hash: Option<String>,
    #[arg(long)]
    pub(super) adapter: Option<String>,
    #[arg(long)]
    pub(super) executor: Option<String>,
    #[arg(long, value_name = "SPEC")]
    pub(super) evidence: Vec<String>,
    #[arg(long, value_name = "PATH")]
    pub(super) ledger: Option<PathBuf>,
    #[arg(last = true, required = true)]
    pub(super) argv: Vec<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum CliPriority {
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
pub(super) enum CliSource {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum CliSubmissionMode {
    Full,
    Legacy,
}

#[derive(Debug, Args)]
pub(super) struct EnqueueArgs {
    #[arg(long = "pool", required = true, action = clap::ArgAction::Append)]
    pub(super) pools: Vec<String>,
    #[arg(long)]
    pub(super) executor: Option<String>,
    #[arg(long, value_enum, default_value = "medium")]
    pub(super) priority: CliPriority,
    #[arg(long, default_value = "shell")]
    pub(super) adapter: String,
    #[arg(long, value_name = "PATH")]
    pub(super) cwd: Option<PathBuf>,
    #[arg(long = "env", value_parser = parse_environment, action = clap::ArgAction::Append)]
    pub(super) environment: Vec<(String, String)>,
    #[arg(long = "pre-prompt-arg", allow_hyphen_values = true, action = clap::ArgAction::Append)]
    pub(super) pre_prompt_argv: Vec<String>,
    #[arg(long)]
    pub(super) approval_policy: Option<String>,
    #[arg(long)]
    pub(super) sandbox_policy: Option<String>,
    #[arg(long)]
    pub(super) model: Option<String>,
    #[arg(long)]
    pub(super) effort: Option<String>,
    #[arg(long)]
    pub(super) workspace_repo: Option<String>,
    #[arg(long)]
    pub(super) workspace_base_rev: Option<String>,
    #[arg(long)]
    pub(super) workspace_branch: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub(super) workspace_worktree: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(super) gate_manifest: Option<PathBuf>,
    #[arg(
        long,
        value_parser = parse_opaque_json,
        allow_hyphen_values = true,
        conflicts_with = "brief_path"
    )]
    pub(super) brief: Option<Value>,
    #[arg(long, value_name = "PATH", conflicts_with = "brief")]
    pub(super) brief_path: Option<PathBuf>,
    #[arg(long = "required-gate", action = clap::ArgAction::Append)]
    pub(super) required_gate_ids: Vec<String>,
    #[arg(long, value_enum)]
    pub(super) acceptance_policy: Option<CliAcceptancePolicy>,
    #[arg(long, value_enum, default_value = "manual")]
    pub(super) source: CliSource,
    #[arg(long)]
    pub(super) dedup_key: Option<String>,
    #[arg(long, value_enum, default_value = "full")]
    pub(super) submission: CliSubmissionMode,
    #[arg(long, value_parser = parse_orchestration, allow_hyphen_values = true)]
    pub(super) orchestration: Option<Orchestration>,
    #[arg(long)]
    pub(super) parent: Option<String>,
    #[arg(long)]
    pub(super) invocation: Option<String>,
    #[arg(long = "evidence", action = clap::ArgAction::Append)]
    pub(super) evidence: Vec<String>,
    #[arg(long, value_parser = parse_opaque_json, allow_hyphen_values = true)]
    pub(super) evidence_class: Option<Value>,
    #[arg(long, allow_hyphen_values = true)]
    pub(super) manifest_hash: Option<String>,
    #[arg(long)]
    pub(super) consumption_estimate: Option<u64>,
    #[arg(long)]
    pub(super) runtime_max_sec: Option<u64>,
    #[arg(long)]
    pub(super) no_enqueue: bool,
    #[arg(long, value_parser = parse_related_trigger, allow_hyphen_values = true)]
    pub(super) related_trigger: Option<RelatedTrigger>,
    #[arg(long)]
    pub(super) wait: bool,
    #[arg(last = true)]
    pub(super) argv: Vec<String>,
}

#[derive(Debug, Args)]
pub(super) struct GcArgs {
    #[arg(long, value_name = "DURATION")]
    pub(super) horizon: String,
    #[arg(long)]
    pub(super) dry_run: bool,
    #[arg(long)]
    pub(super) collect: bool,
    #[arg(long, value_name = "PATH")]
    pub(super) data_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(super) enum ViewCommand {
    Rebuild(ViewRebuildArgs),
}

#[derive(Debug, Args)]
pub(super) struct ViewRebuildArgs {
    #[arg(long, value_name = "DIR")]
    pub(super) data_dir: Option<PathBuf>,
    #[arg(long)]
    pub(super) yes: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum CliAcceptancePolicy {
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

pub(super) fn parse_environment(value: &str) -> Result<(String, String), String> {
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

pub(super) fn parse_opaque_json(value: &str) -> Result<Value, String> {
    serde_json::from_str(value).map_err(|error| format!("invalid JSON value: {error}"))
}

pub(super) fn parse_orchestration(value: &str) -> Result<Orchestration, String> {
    serde_json::from_str(value).map_err(|error| format!("invalid orchestration capsule: {error}"))
}

pub(super) fn parse_related_trigger(value: &str) -> Result<RelatedTrigger, String> {
    let related: RelatedTrigger = serde_json::from_str(value)
        .map_err(|error| format!("invalid related trigger JSON: {error}"))?;
    related
        .validate()
        .map_err(|error| format!("invalid related trigger: {error}"))?;
    Ok(related)
}

#[derive(Debug, Subcommand)]
pub(super) enum QueueCommand {
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
pub(super) enum GhDiagnosticEvent {
    CommandComment,
    Mention,
    Assignment,
    Label,
}

impl GhDiagnosticEvent {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::CommandComment => "command-comment",
            Self::Mention => "mention",
            Self::Assignment => "assignment",
            Self::Label => "label",
        }
    }
}

#[derive(Debug, Subcommand)]
pub(super) enum ProducerCommand {
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
pub(super) enum WitnessCommand {
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
pub(super) enum WitnessVerifyFormat {
    Text,
    Json,
}

#[derive(Debug, Subcommand)]
pub(super) enum LeaseCommand {
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
pub(super) enum DaemonCommand {
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
pub(super) enum QueryCommand {
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
        #[arg(long, alias = "flow-run-id")]
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
        #[arg(long, alias = "flow-run-id")]
        flow_run: Option<String>,
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
        task: Option<String>,
        #[arg(long, alias = "flow-run-id")]
        flow_run: Option<String>,
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
