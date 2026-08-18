use super::*;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(super) enum Mode {
    CheckConfig,
}

#[derive(Debug, Parser)]
#[command(
    name = "tally",
    version = VERSION,
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
    /// The one capability probe `adapter parity` runs on both sides. Hidden
    /// because it is machinery: an operator asks for the comparison, never for
    /// one half of it.
    #[command(name = PARITY_PROBE_VERB, hide = true)]
    ParityProbe(ParityProbeArgs),
    /// Fails on purpose, writing one known line to stderr. The probe's
    /// "can this process see its own error output?" observation is this verb
    /// coming back readable.
    #[command(name = PARITY_FAIL_VERB, hide = true)]
    ParityFail,
    #[command(name = "__producer-dispatch", hide = true)]
    ProducerDispatch(ProducerDispatchArgs),
    Gc(GcArgs),
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    Adapter {
        #[command(subcommand)]
        command: AdapterCommand,
    },
    Campaign {
        #[command(subcommand)]
        command: CampaignCommand,
    },
    /// Replay one run from canonical stores and sampled executor unit facts (`tally rebuild`).
    Rebuild(RebuildArgs),
    /// Validate every commit in a Git revision range against tally's commit grammar.
    LintHistory(LintHistoryArgs),
    /// The judge-tier corpus replay harness: assemble the journaled diagnosis
    /// corpus from the durable record, replay it against a candidate adapter,
    /// and render the disagreement (ETA.md §8.5).
    JudgeReplay {
        #[command(subcommand)]
        command: JudgeReplayCommand,
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
    /// Operator reader-state on flow runs: `archived` and a free-form triage
    /// tag. Never touched by the daemon, a reconciler, or any run — only by
    /// this verb family. See `doc/src/operating/observability.md`.
    ReaderState {
        #[command(subcommand)]
        command: ReaderStateCommand,
    },
}

#[derive(Debug, Args)]
pub(super) struct RebuildArgs {
    /// Flow run whose derived projection should be rebuilt.
    #[arg(value_name = "FLOW_RUN_ID")]
    pub(super) id: String,
    /// Emit the complete machine-readable rebuilt view.
    #[arg(long)]
    pub(super) json: bool,
    /// State directory containing enqueue and execution records; defaults to
    /// the XDG state directory. Point it at the daemon's configured stateDir.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
    /// Data directory containing witness, lifecycle, membership, lineage, and
    /// attestation ledgers; defaults to the XDG data directory.
    #[arg(long, value_name = "PATH")]
    pub(super) data_dir: Option<PathBuf>,
}

/// The two halves of the harness, kept as separate verbs because they are
/// separate acts with separate costs: assembly reads only durable local state
/// and is free, while a run dispatches every case to a model and is the spend
/// the §8.5 decision is bought with. Nothing chains them.
#[derive(Debug, Subcommand)]
pub(super) enum JudgeReplayCommand {
    /// Walk the durable record and emit a corpus of {brief, recorded-verdict}
    /// pairs, reporting what was found and what was unrecoverable.
    Assemble(JudgeReplayAssembleArgs),
    /// Replay an assembled corpus against a candidate adapter and write the
    /// byte-stable disagreement table.
    Run(JudgeReplayRunArgs),
}

#[derive(Debug, Args)]
pub(super) struct JudgeReplayAssembleArgs {
    /// Campaign whose recorded diagnoses join the corpus. Repeat the flag to
    /// sweep several campaigns into one corpus.
    #[arg(long = "campaign", value_name = "NAME", required = true)]
    pub(super) campaigns: Vec<String>,
    /// Content-addressed brief archive holding the dispatched diagnosis briefs;
    /// defaults to `<data-dir>/briefs`.
    #[arg(long, value_name = "PATH")]
    pub(super) briefs: Option<PathBuf>,
    /// Directory holding one attempt-receipt log per campaign; defaults to
    /// `<state-dir>/campaigns/attempt-receipts`.
    #[arg(long, value_name = "PATH")]
    pub(super) receipts_root: Option<PathBuf>,
    /// Corpus directory to write. It must not already hold entries.
    #[arg(long, value_name = "PATH")]
    pub(super) out: PathBuf,
    /// Print the corpus manifest instead of the human summary.
    #[arg(long)]
    pub(super) json: bool,
}

#[derive(Debug, Args)]
pub(super) struct JudgeReplayRunArgs {
    /// Corpus directory written by `judge-replay assemble`.
    #[arg(long, value_name = "PATH")]
    pub(super) corpus: PathBuf,
    /// Candidate adapter name, resolved in the host catalog.
    #[arg(long, value_name = "NAME")]
    pub(super) candidate: String,
    /// File the disagreement table is written to. It is also printed.
    #[arg(long, value_name = "PATH")]
    pub(super) out: PathBuf,
    /// Wall-clock budget for one candidate dispatch.
    #[arg(long, value_name = "SECONDS", default_value_t = DEFAULT_CANDIDATE_TIMEOUT_SEC)]
    pub(super) timeout_sec: u64,
}

#[derive(Debug, Args)]
pub(super) struct LintHistoryArgs {
    /// Git revision or revision range accepted by `git log`.
    #[arg(value_name = "RANGE")]
    pub(super) range: String,
    /// Allowed commit scope. Repeat the flag or pass a comma-separated list.
    #[arg(
        long = "scope",
        visible_alias = "scopes",
        value_name = "SCOPE",
        value_delimiter = ',',
        required = true
    )]
    pub(super) scopes: Vec<String>,
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

#[derive(Debug, Subcommand)]
pub(super) enum CampaignCommand {
    /// Write a minimal admissible worklist for one campaign identity.
    Scaffold(CampaignScaffoldArgs),
    /// Register a repository/worklist campaign and admit its current reconcile pass.
    Arm(CampaignArmArgs),
    /// Append human steering to an armed campaign's local ordered log.
    Steer(CampaignSteerArgs),
    /// Read the typed doubt this campaign is holding for an operator.
    Inbox(CampaignInboxArgs),
    /// Render or execute the release represented by a completed campaign.
    Release(CampaignReleaseArgs),
    /// Reconcile changed armed campaigns into fresh bounded flow passes.
    Poll(CampaignPollArgs),
    /// Show the current or latest durable pass for one campaign, plus usage
    /// across every pass in its lineage.
    Status(CampaignStatusArgs),
    /// Print local campaign identities, admitted digests, and authority bindings.
    List(CampaignListArgs),
    /// Exit successfully only when the local campaign registry is empty.
    Quiescent(CampaignQuiescentArgs),
    /// Remove a local campaign registration.
    Disarm(CampaignDisarmArgs),
}

#[derive(Debug, Args)]
pub(super) struct CampaignScaffoldArgs {
    /// Campaign identity. Names the campaign, the default worklist file, and
    /// the example task the template carries.
    #[arg(value_name = "IDENTITY")]
    pub(super) identity: String,
    /// Write the worklist here instead of `silent-factory-worklists/IDENTITY.json`.
    /// Relative paths resolve against the current directory, and the file must
    /// land inside the checkout that will arm it.
    #[arg(long, value_name = "FILE")]
    pub(super) path: Option<PathBuf>,
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
pub(super) struct CampaignInboxArgs {
    /// Code repository coordinate of an existing armed campaign.
    #[arg(value_name = "OWNER/REPO")]
    pub(super) code_repository: String,
    /// Committed worklist pattern identifying the campaign.
    #[arg(value_name = "WORKLIST")]
    pub(super) worklist_pattern: String,
    /// Emit the complete machine-readable inbox projection.
    #[arg(long)]
    pub(super) json: bool,
    /// Durable registration and attempt-receipt root; defaults beneath tally state.
    #[arg(long, value_name = "PATH")]
    pub(super) state_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignReleaseArgs {
    /// Code repository coordinate of the completed campaign.
    #[arg(value_name = "OWNER/REPO")]
    pub(super) code_repository: String,
    /// Committed worklist pattern identifying the campaign.
    #[arg(value_name = "WORKLIST")]
    pub(super) worklist_pattern: String,
    /// Render the complete release without contacting or changing a forge.
    #[arg(long, conflicts_with = "probe")]
    pub(super) plan: bool,
    /// Exercise the release against a private disposable `tally-probe-*` repository.
    #[arg(long, conflicts_with = "plan")]
    pub(super) probe: bool,
    /// `gh`-compatible forge program used by execute and probe modes. Defaults to `gh` on PATH.
    #[arg(long, value_name = "PATH")]
    pub(super) gh_program: Option<PathBuf>,
    /// Durable registration and attempt-receipt root.
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
    /// Git checkout containing the worklist and campaign code. Defaults to the
    /// current working directory.
    #[arg(long, value_name = "PATH")]
    pub(super) checkout: Option<PathBuf>,
    /// Remote base branch carrying the committed campaign authority.
    #[arg(long, value_name = "BRANCH", default_value = "main")]
    pub(super) base_branch: String,
    /// Named Git remote used to fetch the campaign authority and publish work.
    #[arg(long, value_name = "REMOTE", default_value = "origin")]
    pub(super) remote: String,
    /// Register and validate without admitting a reconcile runner.
    #[arg(long, conflicts_with = "wait")]
    pub(super) no_enqueue: bool,
    /// Wait for this bounded reconcile pass to become terminal.
    #[arg(long)]
    pub(super) wait: bool,
    /// Compatibility actor name carried in the unchanged reconcile brief.
    /// Local steering authorization remains bound to the arming UID.
    #[arg(long = "allow-actor", value_name = "LOGIN")]
    pub(super) allowed_actors: Vec<String>,
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
    /// `retryable-projection`. Recorded in the registration and passed to every
    /// `tally flow run` this campaign dispatches, including the ones `campaign
    /// poll` dispatches later. Defaults to the flow host's 10 s.
    #[arg(long, value_name = "MILLISECONDS")]
    pub(super) projection_wait_ms: Option<u64>,
}

#[derive(Debug, Args)]
pub(super) struct CampaignPollArgs {
    /// Perform one bounded registry scan.
    #[arg(long)]
    pub(super) once: bool,
    /// Wait for each newly admitted reconcile pass to become terminal.
    #[arg(long)]
    pub(super) wait: bool,
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
    /// Run one typed capability probe bare and again as a real job unit, and
    /// put every divergence to the committed containment-rulings table.
    Parity(AdapterParityArgs),
}

#[derive(Debug, Args)]
pub(super) struct AdapterParityArgs {
    /// Adapter the laned half is dispatched through. The probe argv must be
    /// executed literally, so this is only meaningful for an adapter that runs
    /// a command rather than reading a prompt.
    #[arg(long, default_value = "shell", value_name = "NAME")]
    pub(super) adapter: String,
    /// Admission pool; inferred only when a conventional lane is configured.
    #[arg(long)]
    pub(super) pool: Option<String>,
    /// State directory the probe site derives from; defaults to the XDG state
    /// directory. The site is minted under the same `adapter-smoke/` root and
    /// `probe-` prefix the commit probe uses, so `tally gc --state-dir <same
    /// path>` reaps a site a failed probe retained as evidence.
    #[arg(long, value_name = "PATH", conflicts_with = "probe_root")]
    pub(super) state_dir: Option<PathBuf>,
    /// Directory the probe site is created under; defaults to `adapter-smoke/`
    /// below the state directory. Never the system temporary directory: a
    /// hardened adapter's transient unit gets a private /tmp it cannot chdir
    /// into, which would read as a parity defect and be a harness fault.
    #[arg(long, value_name = "PATH")]
    pub(super) probe_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub(super) struct ParityProbeArgs {
    /// Which side this run is. Carried into the report so a pair that is not
    /// one bare and one laned observation is refused rather than compared.
    #[arg(long, value_enum, default_value_t = ProbeSide::Bare)]
    pub(super) side: ProbeSide,
    /// Additionally write the report here. The laned half writes into its
    /// declared workspace so the comparison never depends on the adapter
    /// scrape configuration it is measuring.
    #[arg(long, value_name = "PATH")]
    pub(super) out: Option<PathBuf>,
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
    /// before the node is classified `retryable-projection`. Defaults to 10 s.
    /// Takes precedence over `TALLY_RESULT_PROJECTION_TIMEOUT_MS`.
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
    /// Required. The brief store lives under the daemon data directory; a
    /// fallback to `--state-dir` silently recreates the retired split brief
    /// layout, which the sweep now treats as a legacy store to drain.
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
}

impl From<CliSource> for EnqueueSource {
    fn from(value: CliSource) -> Self {
        match value {
            CliSource::Manual => Self::Manual,
            CliSource::Orchestrator => Self::Orchestrator,
            CliSource::Calendar => Self::Calendar,
            CliSource::EventsDir => Self::EventsDir,
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
    #[command(name = "resume")]
    Unpause {
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
        /// CPUWeight for job units. Optional: a value nobody passed renders
        /// no directive on the unit (vestige-sweep V-1).
        #[arg(long)]
        cpu_weight: Option<u16>,
        /// MemoryMax for job units, in bytes. Optional: a value nobody
        /// passed renders no cap on the unit (vestige-sweep V-1).
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
