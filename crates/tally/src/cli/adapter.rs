use std::time::{SystemTime, UNIX_EPOCH};

use super::text::sanitize_line;
use super::*;

const SMOKE_RUNTIME_MAX_SEC: u64 = 5 * 60;
const CAPTURE_PROJECTION_TIMEOUT: Duration = Duration::from_secs(10);
const CAPTURE_PROJECTION_POLL: Duration = Duration::from_millis(100);
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
        gh_trigger_actor: None,
        gh_self_actor: None,
        gh_origin: None,
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
            })?;
        await_job_with_rearm(client, socket, task_uuid, rpc_timeout)
            .await
            .map_err(|error| retained(error.into()))?
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

    let (captures, missing) =
        await_declared_captures(socket, config_path, &terminal, &required_captures)
            .await
            .map_err(&retained)?;
    let capture_status = if required_captures.is_empty() {
        "not-declared"
    } else if missing.is_empty() {
        "verified"
    } else {
        "missing"
    };
    let outcome = probe
        .as_ref()
        .map(CommitProbe::evaluate)
        .transpose()
        .map_err(&retained)?;
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
    )
    .map_err(&retained)?;
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

fn git(root: &Path, argv: &[&str]) -> Result<String> {
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
    commit_probe: Option<Value>,
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
