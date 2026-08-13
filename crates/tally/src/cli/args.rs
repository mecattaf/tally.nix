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
    #[command(name = "__adapter-smoke-shell", hide = true)]
    AdapterSmokeShell,
    #[command(name = "__adapter-smoke-commit", hide = true)]
    AdapterSmokeCommit,
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
    Adapter {
        #[command(subcommand)]
        command: AdapterCommand,
    },
    Campaign {
        #[command(subcommand)]
        command: CampaignCommand,
    },
    Witness {
        #[command(subcommand)]
        command: WitnessCommand,
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
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    Migrate {
        #[command(subcommand)]
        command: MigrateCommand,
    },
    /// Operator reader-state on flow runs: `archived` and a free-form triage
    /// tag. Never touched by the daemon, a reconciler, or any run — only by
    /// this verb family. See `doc/src/operating/observability.md`.
    ReaderState {
        #[command(subcommand)]
        command: ReaderStateCommand,
    },
}

/// One durable reader-state change, made directly against the store file —
/// no daemon socket involved, the same way `witness verify` reads the
/// witness ledger straight off disk. This is deliberate: the acceptance
/// property under test is that no *daemon* code path can write this file,
/// and a CLI verb that went through the daemon at all would blur that line.
#[derive(Debug, Subcommand)]
pub(super) enum ReaderStateCommand {
    /// Mark a flow run archived. Its jobs and stand-up entries are hidden from
    /// broad `query jobs` / `query standup` views by default; an explicit
    /// `query jobs --flow-run` lookup still returns and annotates its members.
    Archive {
        #[arg(value_name = "FLOW_RUN_ID")]
        flow_run: String,
        /// Set the triage tag at the same time.
        #[arg(long)]
        tag: Option<String>,
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
    /// Clear the archived flag. Leaves any triage tag untouched.
    Unarchive {
        #[arg(value_name = "FLOW_RUN_ID")]
        flow_run: String,
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
    /// Set the free-form triage tag. Leaves the archived flag untouched.
    Tag {
        #[arg(value_name = "FLOW_RUN_ID")]
        flow_run: String,
        #[arg(value_name = "TAG")]
        tag: String,
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
    /// Clear the triage tag. Leaves the archived flag untouched.
    Untag {
        #[arg(value_name = "FLOW_RUN_ID")]
        flow_run: String,
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
    /// Print the reader-state record for one run, or `null` if it has none.
    Show {
        #[arg(value_name = "FLOW_RUN_ID")]
        flow_run: String,
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
}

/// One-shot forward migrations of durable state written by an older binary.
///
/// These are not compatibility shims: the read paths stay strict, and each verb
/// exists so the error that refuses old state can name one documented command
/// instead of leaving an operator to reverse-engineer a naming scheme.
#[derive(Debug, Subcommand)]
pub(super) enum MigrateCommand {
    /// Rewrite pre-label `unit-exit/` records to the labeled unit names the
    /// current binary derives for rows carrying a campaign `taskRef`.
    UnitExitLabels(MigrateUnitExitLabelsArgs),
    /// Move pre-label `capture/` entries to the labeled capture stems the
    /// current binary derives for rows carrying a campaign `taskRef`.
    CaptureLabels(MigrateCaptureLabelsArgs),
}

#[derive(Debug, Args)]
pub(super) struct MigrateUnitExitLabelsArgs {
    /// Tally state directory holding `events/` and `unit-exit/`.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
    /// Write the plan. Without it the plan is printed and nothing changes.
    #[arg(long)]
    pub(super) apply: bool,
}

#[derive(Debug, Args)]
pub(super) struct MigrateCaptureLabelsArgs {
    /// Tally state directory holding `events/` and `capture/`.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
    /// Apply the plan. Without it the plan is printed and nothing moves.
    #[arg(long)]
    pub(super) apply: bool,
}

#[derive(Debug, Subcommand)]
pub(super) enum CampaignCommand {
    /// Register a repository/worklist campaign and admit its current reconcile pass.
    Arm(CampaignArmArgs),
    /// Append human steering to an armed campaign's local ordered log.
    Steer(CampaignSteerArgs),
    /// Pardon an escalated campaign's counters, record why, and re-arm it.
    Resume(CampaignResumeArgs),
    /// Project a worklist into one master issue and native task sub-issues.
    Project(CampaignProjectArgs),
    /// Reconcile changed armed campaigns into fresh bounded flow passes.
    Poll(CampaignPollArgs),
    /// Show the current or latest durable pass for one campaign, plus usage
    /// across every pass in its lineage.
    Status(CampaignStatusArgs),
    /// Print local campaign identities, admitted digests, and authority bindings.
    List(CampaignListArgs),
    /// Exit successfully only when the local campaign registry is empty.
    Quiescent(CampaignQuiescentArgs),
    /// Remove a local campaign registration without changing any forge projection.
    Disarm(CampaignDisarmArgs),
}

#[derive(Debug, Args)]
pub(super) struct CampaignSteerArgs {
    /// Code repository coordinate of an existing armed campaign.
    #[arg(value_name = "OWNER/REPO")]
    pub(super) code_repository: String,
    /// Committed worklist pattern identifying the campaign.
    #[arg(value_name = "WORKLIST")]
    pub(super) worklist_pattern: String,
    /// Address only this task. Omit to steer every task in the campaign.
    #[arg(long, value_name = "TASK_ID")]
    pub(super) task: Option<String>,
    /// Steering text. Use --message-file - when invoking the verb over SSH.
    #[arg(
        long,
        alias = "body",
        value_name = "TEXT",
        conflicts_with = "message_file",
        required_unless_present = "message_file"
    )]
    pub(super) message: Option<String>,
    /// Read steering text from PATH, or from stdin with `-`. The stdin form is
    /// the stable off-host contract: `ssh HOST tally campaign steer ...
    /// --message-file -` does not expose or re-quote the text in remote argv.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "message",
        required_unless_present = "message"
    )]
    pub(super) message_file: Option<PathBuf>,
    /// Durable registration root; defaults beneath tally state.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignResumeArgs {
    /// Code repository coordinate of an existing armed campaign.
    #[arg(value_name = "OWNER/REPO")]
    pub(super) code_repository: String,
    /// Committed worklist pattern identifying the campaign.
    #[arg(value_name = "WORKLIST")]
    pub(super) worklist_pattern: String,
    /// Audit reason recorded on the campaign before its counters are pardoned.
    #[arg(long, value_name = "TEXT")]
    pub(super) reason: String,
    /// Wait for the newly admitted reconcile pass to become terminal.
    #[arg(long)]
    pub(super) wait: bool,
    /// Durable registration root; defaults beneath tally state.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignArmArgs {
    /// Code repository coordinate of the campaign.
    #[arg(value_name = "OWNER/REPO")]
    pub(super) code_repository: String,
    /// Relative committed worklist pattern identifying the campaign.
    #[arg(value_name = "WORKLIST")]
    pub(super) worklist_pattern: String,
    /// Register and validate without admitting a reconcile runner.
    #[arg(long, conflicts_with = "wait")]
    pub(super) no_enqueue: bool,
    /// Wait for this bounded reconcile pass to become terminal.
    #[arg(long)]
    pub(super) wait: bool,
    /// GitHub login whose authored issues/comments may supply campaign input.
    /// Defaults to the currently authenticated gh login; repeat to add actors.
    #[arg(long = "allow-actor", value_name = "LOGIN")]
    pub(super) allowed_actors: Vec<String>,
    /// Permit forge=local for an explicitly test-only campaign.
    #[arg(long)]
    pub(super) allow_test_local_forge: bool,
    /// Override the packaged spec-build flow (primarily for mechanism testing).
    #[arg(long, value_name = "PATH")]
    pub(super) flow: Option<PathBuf>,
    /// Override the packaged policy driver (primarily for mechanism testing).
    #[arg(long, value_name = "PATH")]
    pub(super) driver: Option<PathBuf>,
    /// Durable registration root; defaults beneath tally state.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
    /// Override the per-campaign worktree root.
    #[arg(long, value_name = "PATH")]
    pub(super) workspace_root: Option<PathBuf>,
    /// How long each pass of this campaign waits for a node's advisory
    /// finalMessage projection before classifying the node
    /// `retryable-projection` (#432). Recorded in the registration and passed
    /// to every `tally flow run` this campaign dispatches, including the ones
    /// `campaign poll` dispatches later. Defaults to the flow host's 10 s.
    #[arg(long, value_name = "MILLISECONDS")]
    pub(super) projection_wait_ms: Option<u64>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignProjectArgs {
    /// JSON worklist, optionally carrying a top-level `campaign` object.
    #[arg(value_name = "WORKLIST")]
    pub(super) worklist: PathBuf,
    /// Separate JSON campaign configuration when WORKLIST has none.
    #[arg(long, value_name = "PATH")]
    pub(super) campaign_config: Option<PathBuf>,
    /// GitHub repository receiving the issue graph.
    #[arg(long, value_name = "OWNER/REPO")]
    pub(super) repo: String,
    /// Durable projection root used by a later identity-based arm.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
    /// Existing master issue URL to maintain instead of creating one.
    #[arg(long, value_name = "URL")]
    pub(super) issue: Option<String>,
    /// Master issue title; required only on initial creation when no campaign name exists.
    #[arg(long)]
    pub(super) title: Option<String>,
    /// Label applied to the master issue (created when absent).
    #[arg(long, default_value = "tally-campaign")]
    pub(super) label: String,
    /// Label applied to projected task issues (created when absent).
    #[arg(long, default_value = "tally-campaign-task")]
    pub(super) task_label: String,
}

#[derive(Debug, Args)]
pub(super) struct CampaignPollArgs {
    /// Perform one bounded registry scan.
    #[arg(long)]
    pub(super) once: bool,
    /// Wait for each newly admitted reconcile pass to become terminal.
    #[arg(long)]
    pub(super) wait: bool,
    /// Compatibility input for continuation events emitted by older binaries.
    #[arg(long, value_name = "TOKEN", hide = true)]
    pub(super) continuation_token: Option<String>,
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignStatusArgs {
    /// Code repository coordinate of the campaign.
    #[arg(value_name = "OWNER/REPO")]
    pub(super) code_repository: String,
    /// Committed worklist pattern identifying the campaign.
    #[arg(value_name = "WORKLIST")]
    pub(super) worklist_pattern: String,
    /// Emit the complete machine-readable status object.
    #[arg(long)]
    pub(super) json: bool,
    /// Durable registration root; defaults beneath tally state. A completed
    /// campaign whose registration was pruned resolves from daemon history.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignListArgs {
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignQuiescentArgs {
    /// Durable registration root; defaults beneath tally state.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignDisarmArgs {
    /// Code repository coordinate of the campaign to remove.
    #[arg(value_name = "OWNER/REPO")]
    pub(super) code_repository: String,
    /// Committed worklist pattern identifying the campaign.
    #[arg(value_name = "WORKLIST")]
    pub(super) worklist_pattern: String,
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(super) enum AdapterCommand {
    /// Execute one minimal job through the configured adapter and daemon.
    Smoke(AdapterSmokeArgs),
}

#[derive(Debug, Args)]
pub(super) struct AdapterSmokeArgs {
    /// Configured adapter to execute.
    pub(super) name: String,
    /// Execution working directory; defaults to the current directory. Refused
    /// with --assert-commit, which supplies its own throwaway repository.
    #[arg(long, value_name = "PATH", conflicts_with = "assert_commit")]
    pub(super) cwd: Option<PathBuf>,
    /// Minimal workload passed to agent adapters. Defaults to a one-word reply,
    /// or to a write-stage-commit workload under --assert-commit.
    #[arg(long, allow_hyphen_values = true)]
    pub(super) prompt: Option<String>,
    /// Admission pool; inferred only when a conventional lane is configured.
    #[arg(long)]
    pub(super) pool: Option<String>,
    /// Named adapter sandbox policy to launch this smoke under.
    #[arg(long, value_name = "NAME")]
    pub(super) sandbox: Option<String>,
    /// Named adapter approval policy to launch this smoke under.
    #[arg(long, value_name = "NAME")]
    pub(super) approval_policy: Option<String>,
    /// Run the adapter in a throwaway git repository and require it to leave one
    /// commit descended from the seeded base and a clean worktree.
    #[arg(long)]
    pub(super) assert_commit: bool,
    /// State directory the --assert-commit probe root derives from; defaults to
    /// the XDG state directory. Point this at the daemon's configured stateDir
    /// so that `tally gc --state-dir <same path>` reaps the probe repositories
    /// a failed smoke retains. On a NixOS deployment the two are not the same
    /// by default: the module's stateDir is /var/lib/tally/state while an
    /// operator's shell resolves $XDG_STATE_HOME/tally.
    #[arg(long, value_name = "PATH", conflicts_with = "probe_root")]
    pub(super) state_dir: Option<PathBuf>,
    /// Directory the --assert-commit probe repository is created under; defaults
    /// to adapter-smoke/ below the state directory. Name the campaign's
    /// workspace root to probe where implementation nodes actually run — but a
    /// probe seeded outside <gc state dir>/adapter-smoke/ is not swept by
    /// `tally gc` and must be removed by hand. Never the system temporary
    /// directory: a hardened adapter's transient unit gets a private /tmp, and
    /// an agent sandbox may treat it as writable by default.
    #[arg(long, value_name = "PATH", requires = "assert_commit")]
    pub(super) probe_root: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(super) enum HistoryCommand {
    /// Offline lifecycle-history compaction: drop records older than the
    /// retention window and record the cut in the durable retention metadata.
    /// Durable enqueue events are recovery inputs and are never touched.
    Compact(HistoryCompactArgs),
}

#[derive(Debug, Args)]
pub(super) struct HistoryCompactArgs {
    /// Keep lifecycle records from the newest KEEP_DAYS days.
    #[arg(long, value_name = "DAYS")]
    pub(super) keep_days: u32,
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(super) data_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub(super) enum FlowCommand {
    Run(FlowRunArgs),
    Check(FlowCheckArgs),
    Render(FlowRenderArgs),
    Cancel(FlowCancelArgs),
    /// Record that a terminal run is replaced by a fresh successor run.
    ///
    /// The old run and its history are preserved unchanged. Repeating the exact
    /// same call is safe, so a supervisor may retry it after its own restart.
    Supersede(FlowSupersedeArgs),
}

#[derive(Debug, Args)]
pub(super) struct FlowSupersedeArgs {
    /// The terminal run being retired.
    #[arg(long, value_name = "UUID")]
    pub(super) flow_run_id: String,
    /// The fresh run that replaces it. It must not have started yet.
    #[arg(long, value_name = "UUID")]
    pub(super) new_flow_run_id: String,
    /// Why the old run was abandoned; recorded durably for later audit.
    #[arg(long, value_enum)]
    pub(super) reason: SupersedeReasonArg,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
#[clap(rename_all = "kebab-case")]
pub(super) enum SupersedeReasonArg {
    /// A declarative activation moved the script and/or argument store paths.
    GenerationChange,
    ScriptChanged,
    ArgsChanged,
    CatalogChanged,
    Operator,
}

impl SupersedeReasonArg {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::GenerationChange => "generation-change",
            Self::ScriptChanged => "script-changed",
            Self::ArgsChanged => "args-changed",
            Self::CatalogChanged => "catalog-changed",
            Self::Operator => "operator",
        }
    }
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
    #[arg(
        long,
        value_parser = parse_opaque_json,
        allow_hyphen_values = true,
        conflicts_with_all = ["args_path", "args_from_brief"]
    )]
    pub(super) args: Option<Value>,
    /// Read flow arguments from an absolute JSON file instead of argv.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = ["args", "args_from_brief"]
    )]
    pub(super) args_path: Option<PathBuf>,
    /// Read flow arguments from the private file named by TALLY_BRIEF.
    #[arg(long, conflicts_with_all = ["args", "args_path"])]
    pub(super) args_from_brief: bool,
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
    /// How long a terminal node waits for its advisory finalMessage projection
    /// before the node is classified `retryable-projection` (#432). Defaults to
    /// 10 s. Takes precedence over `TALLY_RESULT_PROJECTION_TIMEOUT_MS`.
    #[arg(long, value_name = "MILLISECONDS")]
    pub(super) result_projection_wait_ms: Option<u64>,
}

#[derive(Debug, Args)]
pub(super) struct FlowCheckArgs {
    #[arg(value_name = "SCRIPT")]
    pub(super) script: PathBuf,
    #[arg(
        long,
        value_parser = parse_opaque_json,
        allow_hyphen_values = true,
        conflicts_with = "args_path"
    )]
    pub(super) args: Option<Value>,
    /// Read flow arguments from an absolute JSON file instead of argv.
    #[arg(long, value_name = "PATH", conflicts_with = "args")]
    pub(super) args_path: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub(super) catalog: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct FlowRenderArgs {
    #[arg(value_name = "SCRIPT")]
    pub(super) script: PathBuf,
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
    /// Required. The brief store lives under the daemon data directory; the
    /// former fallback to `--state-dir` silently recreated the split brief
    /// layout #271 retired, and the sweep now treats that layout as a legacy
    /// store to drain.
    #[arg(long, value_name = "PATH")]
    pub(super) data_dir: PathBuf,
    #[arg(long, hide = true)]
    pub(super) engine_only: bool,
}

#[derive(Debug, Args)]
pub(super) struct RecordUnitExitArgs {
    #[arg(long, value_name = "PATH")]
    pub(super) record: PathBuf,
    #[arg(long)]
    pub(super) unit: String,
    /// The `systemctl` program the accounting probe invokes. Defaults to
    /// resolving `systemctl` on `PATH`; overridable so a test double can
    /// stand in for the real binary the way `Executor::with_systemctl`
    /// already lets the daemon's own liveness probe be faked.
    #[arg(long, value_name = "PATH", default_value = "systemctl")]
    pub(super) systemctl: PathBuf,
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

/// Task states a campaign board can be narrowed to. A frozen worklist may hold
/// 128 tasks, and an operator checking a run usually wants one of these slices.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum RunTaskFilter {
    Done,
    Running,
    Blocked,
    Pending,
}

impl RunTaskFilter {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Running => "running",
            Self::Blocked => "blocked",
            Self::Pending => "pending",
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
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
    /// Skip the state-directory pruners entirely and sweep GC roots only.
    #[arg(long)]
    pub(super) skip_state_dir: bool,
    #[arg(
        long,
        value_name = "DURATION",
        default_value = tally_core::retention::DEFAULT_CAPTURE_ARCHIVE_MAX_AGE
    )]
    pub(super) capture_archive_horizon: String,
    #[arg(
        long,
        value_name = "DURATION",
        default_value = tally_core::retention::DEFAULT_EVENTS_DONE_MAX_AGE
    )]
    pub(super) events_done_horizon: String,
    #[arg(
        long,
        value_name = "DURATION",
        default_value = tally_core::retention::DEFAULT_EVENTS_REJECTED_MAX_AGE
    )]
    pub(super) events_rejected_horizon: String,
    #[arg(
        long,
        value_name = "COUNT",
        default_value_t = tally_core::retention::DEFAULT_EVENTS_REJECTED_MAX_COUNT
    )]
    pub(super) events_rejected_max_count: usize,
    #[arg(
        long,
        value_name = "DURATION",
        default_value = tally_core::retention::DEFAULT_PRODUCER_MARKER_MAX_AGE
    )]
    pub(super) producer_marker_horizon: String,
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
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
    Poll {
        name: String,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        no_enqueue: bool,
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
    Explain {
        name: String,
        #[arg(long)]
        item: String,
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
    /// List every forge projection whose producer is no longer configured.
    ///
    /// These are terminal: the task completions are settled and witnessed, and
    /// only the forge-side projection was lost. Reading them needs the state
    /// directory alone — the configuration no longer names the producer.
    Orphaned {
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
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
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
        /// Emit a single page verbatim; the caller owns `nextCursor`. Without
        /// it the command follows the cursor and prints the whole window.
        #[arg(long)]
        json: bool,
        /// Include jobs whose creating run is archived operator reader-state
        /// in a broad query. An explicit `--flow-run` lookup always includes
        /// its archived members and therefore conflicts with this control.
        #[arg(long, conflicts_with_all = ["no_archived", "flow_run"])]
        archived: bool,
        /// Explicit spelling of the default: hide jobs whose creating run is
        /// archived in a broad query. Explicit flow-run lookups never hide.
        #[arg(long, conflicts_with = "flow_run")]
        no_archived: bool,
    },
    Job {
        id: String,
    },
    /// Print the generation lineage of one flow run: what it superseded, what
    /// superseded it, and which run in the chain is current.
    Lineage {
        #[arg(value_name = "FLOW_RUN_ID")]
        id: String,
    },
    Run {
        id: String,
        #[arg(long)]
        json: bool,
        /// Show only task rows in this state. Counts stay whole-run.
        #[arg(long, value_name = "STATE")]
        status: Option<RunTaskFilter>,
        /// Read the run from the durable stores on disk instead of asking the
        /// daemon. The same view is produced automatically when a live read
        /// exceeds its RPC deadline; this asks for it without trying the
        /// daemon at all. It is labelled, may be stale, and shows no in-flight
        /// execution state.
        #[arg(long)]
        durable: bool,
        /// State directory the durable view reads enqueue events from; defaults
        /// to the XDG state directory. Point it at the daemon's configured
        /// stateDir.
        #[arg(long, value_name = "PATH")]
        state_dir: Option<PathBuf>,
        /// Data directory the durable view reads the witness ledger, lifecycle
        /// history, and membership from; defaults to the XDG data directory.
        #[arg(long, value_name = "PATH")]
        data_dir: Option<PathBuf>,
    },
    Status {
        #[arg(long)]
        pool: Option<String>,
    },
    /// Show daemon-owned storage usage, budgets, and growth.
    Storage,
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
        /// Durable lifecycle-stream position, taken from the `position` field
        /// of a previous response. Returns only events after it. This is a
        /// stream coordinate, not a time filter (`--since`) and not an
        /// ephemeral page cursor (`--cursor`).
        #[arg(long, value_name = "POSITION")]
        after: Option<String>,
        /// Emit the structured lifecycle envelope instead of human lines.
        #[arg(long)]
        json: bool,
        /// Preserve journal, evidence, and witness echoes as separate records.
        #[arg(long)]
        provenance: bool,
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
        /// Include entries and runs archived as operator reader-state. The
        /// default hides them and reports how many it hid.
        #[arg(long, conflicts_with = "no_archived")]
        archived: bool,
        /// Explicit spelling of the default: hide archived entries and runs.
        #[arg(long)]
        no_archived: bool,
    },
    Pools,
}
