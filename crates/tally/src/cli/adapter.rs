use std::time::{SystemTime, UNIX_EPOCH};

use super::text::sanitize_line;
use super::*;

const SMOKE_RUNTIME_MAX_SEC: u64 = 5 * 60;
/// How long the smoke keeps asking for a capture the daemon has answered about
/// but not yet projected. This is the *projection* window, not an RPC deadline:
/// it bounds "the daemon replied and the capture is not there yet", which ends
/// in a missing-capture FAIL. The deadline for the reply itself is
/// `--rpc-timeout-sec` (see [`run_adapter_smoke`]), and a reply that never
/// arrives is a different outcome with a different exit code.
const CAPTURE_PROJECTION_TIMEOUT: Duration = Duration::from_secs(10);
const CAPTURE_PROJECTION_POLL: Duration = Duration::from_millis(100);
/// The smoke could not read its verdict. Distinct from 1 ("the adapter, or
/// something the smoke asserts about it, failed") because the two demand
/// opposite next actions, and conflating them cost real diagnosis time on
/// 2026-08-07: two smokes whose daemon-side verdicts were exit 0 and
/// witness-emitted PASS were reported as failures because a `query.job` read
/// timed out during a daemon stall (#431). A timed-out read is never rendered
/// as adapter failure.
pub(super) const VERDICT_UNAVAILABLE_EXIT: i32 = 5;
const DEFAULT_SMOKE_PROMPT: &str = "Reply with the single word ok.";
pub(super) const COMMIT_PROBE_FILE: &str = "tally-commit-probe.txt";
pub(super) const COMMIT_PROBE_MESSAGE: &str = "tally commit probe";
const COMMIT_PROBE_BRANCH: &str = "adapter-smoke-probe";
const COMMIT_PROBE_REPO: &str = "tally/adapter-smoke";
const COMMIT_PROBE_PROMPT: &str = concat!(
    "Create a file named tally-commit-probe.txt in the current directory whose ",
    "only contents are the word ok. Then stage it and commit it with the ",
    "message \"tally commit probe\". Change nothing else and leave the worktree ",
    "clean. Reply with the single word done.",
);

/// What the smoke is able to say about the run it just asked for.
///
/// Three-valued on purpose. `Pass` and `Fail` are claims about the adapter;
/// `Unavailable` is a claim about the smoke's own reach, and it is never a
/// claim about the adapter. The rendered label is what the operator reads and
/// what a wrapper greps for, so it is pinned here rather than spelled at each
/// print site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SmokeVerdict {
    Pass,
    Fail,
    /// The result read did not return within its deadline. The adapter may
    /// have passed, failed, or still be running; this states only that the
    /// daemon did not answer.
    Unavailable,
}

impl SmokeVerdict {
    const fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Unavailable => "VERDICT-UNAVAILABLE",
        }
    }
}

/// Whether this failure is "the daemon did not answer in time" as opposed to
/// "the daemon answered, and the answer was an error".
///
/// Both deadline variants count: a per-call deadline and the reconnect window
/// used while re-arming a wait describe the same fact from the client's side —
/// no reply arrived — and a stalled daemon can produce either.
pub(super) fn is_rpc_timeout(error: &WireIoError) -> bool {
    matches!(
        error,
        WireIoError::DeadlineExceeded { .. } | WireIoError::RearmDeadlineExceeded { .. }
    )
}

pub(super) fn verdict_unavailable(detail: String) -> anyhow::Error {
    exit_failure(VERDICT_UNAVAILABLE_EXIT, detail)
}

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
        AdapterCommand::Parity(args) => {
            run_adapter_parity(socket, config_path, rpc_timeout, args).await
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
    let prompt = args.prompt.unwrap_or_else(|| {
        if args.assert_commit {
            COMMIT_PROBE_PROMPT.to_owned()
        } else {
            DEFAULT_SMOKE_PROMPT.to_owned()
        }
    });
    if prompt.trim().is_empty() || prompt.contains('\0') {
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
    // A policy name the adapter never declared renders no argv at all, so it
    // would silently smoke the adapter's own defaults instead of the pairing
    // the operator asked about.
    if let Some(policy) = args.approval_policy.as_deref() {
        if !adapter.launch.approval_policies.contains_key(policy) {
            return Err(invalid(format!(
                "adapter {:?} declares no approval policy {policy:?}; declared: {}",
                args.name,
                configured_names(adapter.launch.approval_policies.keys())
            )));
        }
    }
    if let Some(policy) = args.sandbox.as_deref() {
        if !adapter.launch.sandbox_policies.contains_key(policy) {
            return Err(invalid(format!(
                "adapter {:?} declares no sandbox policy {policy:?}; declared: {}",
                args.name,
                configured_names(adapter.launch.sandbox_policies.keys())
            )));
        }
    }
    let pool = resolve_smoke_pool(&args.name, args.pool.as_deref(), &config.pools)?;
    // Connect before seeding. Everything up to here — an unknown adapter, an
    // undeclared policy, an unresolvable pool, an unreachable daemon — has
    // nothing to say about the adapter under test, and a probe repository
    // created before those checks is a git tree nobody asked for and nobody
    // reaps. Seeding after the connect means the whole class of "could not even
    // reach the daemon" failures leaves nothing behind.
    let client = connect_rpc(socket, config_path).await?;
    let probe = if args.assert_commit {
        let parent = match args.probe_root {
            Some(root) => root,
            None => CommitProbe::root_under(&match args.state_dir {
                Some(state_dir) => state_dir,
                None => default_state_dir()?,
            }),
        };
        Some(CommitProbe::seed(&parent)?)
    } else {
        None
    };
    // Past this point a failure has left a repository on disk. Retaining it is
    // deliberate — a failed probe is the evidence — but the same retention
    // applies to failures that say nothing about the adapter, and an operator
    // cannot collect evidence they were never told the location of. Every error
    // from here down names the path.
    let retained = |error: anyhow::Error| match &probe {
        Some(probe) => error.context(format!(
            "commit probe repository retained at {}",
            probe.root.display()
        )),
        None => error,
    };
    let cwd = match &probe {
        Some(probe) => probe.root.clone(),
        None => resolve_smoke_cwd(args.cwd)?,
    };
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
            if args.assert_commit {
                "__adapter-smoke-commit".to_owned()
            } else {
                "__adapter-smoke-shell".to_owned()
            },
        ]
    } else {
        vec![prompt]
    };
    let adapter_options = AdapterJobOptions {
        approval_policy: args.approval_policy.clone(),
        sandbox_policy: args.sandbox.clone(),
        ..AdapterJobOptions::default()
    };
    let payload = EnqueuePayload {
        invocation: None,
        argv: Some(workload_argv),
        pools: Some(vec![pool.clone()]),
        executor: None,
        priority: Some(Priority::Medium),
        adapter: Some(args.name.clone()),
        cwd: Some(cwd.clone()),
        workspace: probe.as_ref().map(CommitProbe::workspace),
        adapter_options: (!adapter_options.is_default()).then_some(adapter_options),
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
        task_uuid: None,
        related_trigger: None,
        wait: true,
    };

    let admitted = client
        .call(
            "queue.enqueue",
            Some(serde_json::to_value(payload).map_err(|error| retained(error.into()))?),
        )
        .await
        .map_err(|error| retained(error.into()))?;
    report_degraded_membership(&admitted).map_err(retained)?;
    let terminal = if admitted.get("verdict").and_then(Value::as_str).is_some() {
        admitted
    } else {
        let task_uuid = admitted
            .get("task_uuid")
            .and_then(Value::as_str)
            .filter(|task_uuid| !task_uuid.is_empty())
            .ok_or_else(|| {
                retained(invalid(
                    "queue.enqueue returned no task_uuid for adapter smoke",
                ))
            })?
            .to_owned();
        match await_job_with_rearm(client, socket, &task_uuid, rpc_timeout).await {
            Ok(terminal) => terminal,
            Err(error) if is_rpc_timeout(&error) => {
                // The job was admitted and the wait did not return. Nothing
                // here is a statement about the adapter, so the receipt names
                // the task and says so, and the probe repository is retained
                // rather than judged.
                print_smoke_result(
                    &args.name,
                    &label,
                    &pool,
                    &cwd,
                    &json!({"task_uuid": task_uuid}),
                    &required_captures,
                    &BTreeMap::new(),
                    "not-read",
                    probe.as_ref().map(CommitProbe::not_checked),
                    SmokeVerdict::Unavailable,
                    rpc_timeout,
                )
                .map_err(&retained)?;
                return Err(retained(verdict_unavailable(format!(
                    "adapter smoke {:?} could not read its verdict: {} did not return within {} s; the daemon may be stalled (see #431). Task {task_uuid} was admitted and its verdict, if any, is on the daemon side",
                    args.name,
                    "queue.await_job",
                    rpc_timeout.as_secs(),
                ))));
            }
            Err(error) => return Err(retained(error.into())),
        }
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
            probe.as_ref().map(CommitProbe::not_checked),
            SmokeVerdict::Fail,
            rpc_timeout,
        )
        .map_err(&retained)?;
        print_captured_stderr(&args.name, &terminal).map_err(&retained)?;
        let verdict = terminal
            .get("verdict")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(retained(exit_failure(
            exit_code,
            format!(
                "adapter smoke {:?} finished with verdict {verdict}",
                args.name
            ),
        )));
    }

    let read = await_declared_captures(
        socket,
        config_path,
        &terminal,
        &required_captures,
        rpc_timeout,
    )
    .await
    .map_err(&retained)?;
    let (captures, missing) = match &read {
        CaptureRead::Read { captures, missing } => (captures.clone(), missing.clone()),
        CaptureRead::Unavailable => (BTreeMap::new(), Vec::new()),
    };
    let capture_status = match &read {
        CaptureRead::Unavailable => "unavailable",
        CaptureRead::Read { .. } if required_captures.is_empty() => "not-declared",
        CaptureRead::Read { missing, .. } if missing.is_empty() => "verified",
        CaptureRead::Read { .. } => "missing",
    };
    // Evaluated in every outcome because it is a filesystem fact and needs no
    // daemon, but it is only *judged* below when a verdict exists: a probe
    // status cannot complete a verdict whose other half never arrived.
    let outcome = probe
        .as_ref()
        .map(CommitProbe::evaluate)
        .transpose()
        .map_err(&retained)?;
    let verdict = match &read {
        CaptureRead::Unavailable => SmokeVerdict::Unavailable,
        CaptureRead::Read { missing, .. } => {
            let probe_verified = outcome.as_ref().is_none_or(|outcome| {
                outcome.get("status").and_then(Value::as_str) == Some("verified")
            });
            if missing.is_empty() && probe_verified {
                SmokeVerdict::Pass
            } else {
                SmokeVerdict::Fail
            }
        }
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
        outcome.clone(),
        verdict,
        rpc_timeout,
    )
    .map_err(&retained)?;
    if verdict == SmokeVerdict::Unavailable {
        // The execution verdict was read and the capture projection was not,
        // so the smoke's own verdict is incomplete. A probe repository, if
        // any, is retained rather than discarded: nothing here established
        // that it may go.
        return Err(retained(verdict_unavailable(format!(
            "adapter smoke {:?} could not read its verdict: query.job did not return within {} s; the daemon may be stalled (see #431). The adapter's execution verdict was {}",
            args.name,
            rpc_timeout.as_secs(),
            terminal
                .get("verdict")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
        ))));
    }
    let mut discarded = false;
    if let (Some(probe), Some(outcome)) = (&probe, &outcome) {
        let status = outcome["status"].as_str().unwrap_or("unknown");
        if status != "verified" {
            return Err(exit_failure(
                1,
                format!(
                    "adapter smoke {:?} ran under sandboxPolicy {} but left no publishable commit ({status}); probe repository retained at {}",
                    args.name,
                    args.sandbox.as_deref().unwrap_or("<adapter default>"),
                    probe.root.display()
                ),
            ));
        }
        probe.discard();
        discarded = true;
    }
    if missing.is_empty() {
        Ok(())
    } else {
        let error = exit_failure(
            1,
            format!(
                "adapter smoke {:?} passed execution but did not project declared capture(s) {} within {} seconds",
                args.name,
                missing.join(", "),
                CAPTURE_PROJECTION_TIMEOUT.as_secs()
            ),
        );
        // A verified probe has already been removed, so there is no path left to
        // name; an unverified one returned above with its own message.
        Err(if discarded { error } else { retained(error) })
    }
}

/// A throwaway git repository handed to one adapter run so the question
/// "can this adapter commit under these policies?" is answered by the real
/// binary's filesystem behaviour rather than by the argv tally meant to emit.
///
/// The repository is deliberately not in the system temporary directory. A
/// hardened adapter's transient unit runs under `PrivateTmp=yes`, where a `/tmp`
/// working directory does not exist inside the namespace and systemd kills the
/// unit with an empty capture — a harness failure that reads exactly like a
/// policy failure. An agent sandbox may also treat `$TMPDIR` and `/tmp` as
/// default writable roots, which would let a confining policy pass a probe it
/// should fail. The probe is carried as workspace metadata for the same reason a
/// campaign implementation node is: that is what puts its worktree in the unit's
/// `ReadWritePaths=` under every hardening tier, without weakening any of them.
struct CommitProbe {
    root: PathBuf,
    base_rev: String,
}

impl CommitProbe {
    /// The probe root for a given state directory. Deriving it from the state
    /// directory rather than from `std::env::temp_dir()` is the whole point: a
    /// hardened adapter's transient unit gets a private `/tmp` it cannot chdir
    /// into, and an agent sandbox may treat `$TMPDIR` as writable by default.
    ///
    /// This is the *same* derivation `tally gc` sweeps
    /// (`retention::ADAPTER_SMOKE_DIRECTORY` under the state directory it is
    /// given), so handing both commands the same `--state-dir` makes the
    /// producer of these repositories and their reaper name one place. Without
    /// `--state-dir` the CLI resolves the XDG state directory, which on a NixOS
    /// deployment is not the module's `stateDir` — that is why the flag exists.
    fn root_under(state_dir: &Path) -> PathBuf {
        state_dir.join(tally_core::retention::ADAPTER_SMOKE_DIRECTORY)
    }

    fn seed(parent: &Path) -> Result<Self> {
        if !parent.is_absolute() {
            return Err(invalid(format!(
                "commit probe root must be an absolute path: {}",
                parent.display()
            )));
        }
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create commit probe root {}", parent.display()))?;
        let root = parent.join(format!(
            "{}{}-{}",
            tally_core::retention::ADAPTER_SMOKE_PROBE_PREFIX,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir(&root)
            .with_context(|| format!("cannot create commit probe repository {}", root.display()))?;
        let seed = root.join("README.md");
        std::fs::write(&seed, "tally adapter smoke commit probe\n")
            .context("cannot seed commit probe repository")?;
        // Identity and signing are configured locally so the probe never
        // depends on, and never writes to, the operator's global git config.
        for argv in [
            vec!["init", "--quiet", "--initial-branch", COMMIT_PROBE_BRANCH],
            vec!["config", "user.email", "adapter-smoke@localhost"],
            vec!["config", "user.name", "tally adapter smoke"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["add", "--all"],
            vec!["commit", "--quiet", "--message", "commit probe base"],
        ] {
            git(&root, &argv)?;
        }
        let base_rev = git(&root, &["rev-parse", "HEAD"])?;
        Ok(Self { root, base_rev })
    }

    /// Workspace metadata naming this repository. A campaign implementation node
    /// reaches its worktree through exactly this field, so declaring it is what
    /// makes the probe writable under `ProtectSystem=strict` rather than an
    /// exception carved for probes.
    fn workspace(&self) -> WorkspaceMetadata {
        WorkspaceMetadata {
            repo: COMMIT_PROBE_REPO.to_owned(),
            base_rev: self.base_rev.clone(),
            branch: COMMIT_PROBE_BRANCH.to_owned(),
            worktree_path: self.root.clone(),
        }
    }

    fn not_checked(&self) -> Value {
        json!({
            "status": "not-checked",
            "repository": self.root,
            "baseRev": self.base_rev,
        })
    }

    fn evaluate(&self) -> Result<Value> {
        let head_rev = git(&self.root, &["rev-parse", "HEAD"])?;
        let dirty = git(&self.root, &["status", "--porcelain"])?;
        let descends = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["merge-base", "--is-ancestor", &self.base_rev, &head_rev])
            .status()
            .context("cannot run git merge-base in the commit probe repository")?
            .success();
        let commits = if head_rev == self.base_rev || !descends {
            0
        } else {
            git(
                &self.root,
                &[
                    "rev-list",
                    "--count",
                    &format!("{}..{head_rev}", self.base_rev),
                ],
            )?
            .parse::<u64>()
            .unwrap_or_default()
        };
        let status = if head_rev == self.base_rev {
            "no-commit"
        } else if !descends {
            "unrelated-history"
        } else if !dirty.is_empty() {
            "dirty-worktree"
        } else {
            "verified"
        };
        Ok(json!({
            "status": status,
            "repository": self.root,
            "baseRev": self.base_rev,
            "headRev": head_rev,
            "commits": commits,
            "worktreeStatus": dirty.lines().map(sanitize_line).collect::<Vec<_>>(),
        }))
    }

    /// Only a verified probe is discarded; a failed one is the evidence.
    fn discard(&self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(super) fn git(root: &Path, argv: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(argv)
        .output()
        .with_context(|| format!("cannot run git {} in {}", argv.join(" "), root.display()))?;
    if !output.status.success() {
        return Err(invalid(format!(
            "git {} failed in the commit probe repository: {}",
            argv.join(" "),
            sanitize_line(String::from_utf8_lossy(&output.stderr).trim())
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// The `shell` adapter's built-in commit workload: the positive control for
/// `adapter smoke --assert-commit`, and the failure-free half of the probe.
pub(super) fn run_adapter_smoke_commit() -> Result<()> {
    let cwd = std::env::current_dir().context("cannot resolve commit workload directory")?;
    std::fs::write(cwd.join(COMMIT_PROBE_FILE), "ok\n")
        .context("commit workload could not write its probe file")?;
    git(&cwd, &["add", "--all"])?;
    git(
        &cwd,
        &["commit", "--quiet", "--message", COMMIT_PROBE_MESSAGE],
    )?;
    outln!("ok");
    Ok(())
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

pub(super) fn resolve_smoke_pool(
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

pub(super) fn configured_names<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let names = names.map(String::as_str).collect::<Vec<_>>();
    if names.is_empty() {
        "<none>".to_owned()
    } else {
        names.join(", ")
    }
}

/// The outcome of the smoke's result read, kept apart from its contents.
///
/// A read that returned and found nothing is a different fact from a read that
/// never returned, and collapsing the two is the defect this type exists to
/// make unrepresentable.
enum CaptureRead {
    Read {
        captures: BTreeMap<String, Value>,
        missing: Vec<String>,
    },
    /// `query.job` exceeded its deadline. Says nothing about the captures.
    Unavailable,
}

/// Poll the daemon until every declared capture is projected, the projection
/// window closes, or the read itself stops returning.
///
/// `rpc_timeout` is the operator's `--rpc-timeout-sec` / `TALLY_RPC_TIMEOUT_SEC`
/// and it is the deadline for each `query.job` reply. It used to be
/// [`CAPTURE_PROJECTION_TIMEOUT`], a private 10 s constant no flag could
/// reach, so the one knob an operator had did not govern the one read that
/// timed out under a stall. The projection window stays its own bound: it
/// answers "not projected yet", the deadline answers "not answered at all".
async fn await_declared_captures(
    socket: &Path,
    config_path: Option<&Path>,
    terminal: &Value,
    required: &[String],
    rpc_timeout: Duration,
) -> Result<CaptureRead> {
    if required.is_empty() {
        return Ok(CaptureRead::Read {
            captures: BTreeMap::new(),
            missing: Vec::new(),
        });
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
        let result = match client
            .call_with_deadline("query.job", Some(json!({"id": task_uuid})), rpc_timeout)
            .await
        {
            Ok(result) => result,
            Err(error) if is_rpc_timeout(&error) => return Ok(CaptureRead::Unavailable),
            Err(error) => return Err(error.into()),
        };
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
            return Ok(CaptureRead::Read { captures, missing });
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
    commit_probe: Option<Value>,
    verdict_state: SmokeVerdict,
    rpc_timeout: Duration,
) -> Result<()> {
    let field = |snake: &str, camel: &str| {
        terminal
            .get(snake)
            .or_else(|| terminal.get(camel))
            .cloned()
            .unwrap_or(Value::Null)
    };
    outln!(
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
            "commitProbe": commit_probe,
            // The smoke's own three-valued verdict, beside the daemon's
            // `verdict` for the job. They answer different questions: the
            // daemon's is what the run did, this one is what the smoke can
            // state about it, and only this one can be VERDICT-UNAVAILABLE.
            "verdictState": verdict_state.label(),
            // The deadline this run actually used, so the receipt states the
            // knob rather than leaving an operator to infer it.
            "rpcTimeoutSec": rpc_timeout.as_secs(),
        }))?
    );
    Ok(())
}

fn print_captured_stderr(adapter: &str, terminal: &Value) -> Result<()> {
    let excerpt = terminal
        .get("stderr_excerpt")
        .or_else(|| terminal.get("stderrExcerpt"))
        .and_then(Value::as_str)
        .filter(|excerpt| !excerpt.is_empty());
    match excerpt {
        Some(excerpt) => {
            errln!("adapter smoke {adapter:?} captured stderr:");
            // The excerpt is whatever the adapter wrote; printing it verbatim
            // hands control of the operator's terminal to a failing job.
            for line in excerpt.lines() {
                errln!("{}", sanitize_line(line));
            }
        }
        None => errln!("adapter smoke {adapter:?} captured stderr was empty"),
    }
    Ok(())
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
    fn commit_probe_seeds_a_worktree_it_declares_as_its_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("adapter-smoke");
        let probe = CommitProbe::seed(&root).unwrap();

        // Seeding creates the probe root, not just the repository, so an
        // operator may name a campaign workspace root that does not exist yet.
        assert!(probe.root.starts_with(&root));
        assert!(probe.root.join(".git").is_dir());

        // The worktree is declared as workspace metadata. That declaration is
        // the only per-job mechanism that puts a directory in a hardened
        // transient unit's ReadWritePaths=, and it is the same one a campaign
        // implementation node reaches its worktree through.
        let workspace = probe.workspace();
        assert_eq!(workspace.worktree_path, probe.root);
        assert_eq!(workspace.base_rev, probe.base_rev);
        assert_eq!(workspace.branch, COMMIT_PROBE_BRANCH);
        workspace.validate().unwrap();

        // A seeded probe has not committed anything yet.
        assert_eq!(probe.evaluate().unwrap()["status"], "no-commit");

        // The default root is derived from the state directory. It is never the
        // system temporary directory: a hardened unit gets a private /tmp it
        // cannot chdir into, and an agent sandbox may treat $TMPDIR as writable.
        assert_eq!(
            CommitProbe::root_under(Path::new("/var/lib/tally")),
            Path::new("/var/lib/tally/adapter-smoke")
        );

        // A relative root would resolve against whatever directory the daemon
        // happens to run in rather than the one the operator named.
        assert!(CommitProbe::seed(Path::new("relative/probe")).is_err());
    }

    #[test]
    fn unrelated_or_absent_pool_requires_an_override() {
        assert!(resolve_smoke_pool("shell", None, &pools(&["worker"])).is_err());
        assert!(resolve_smoke_pool("shell", None, &pools(&[])).is_err());
    }
}
