use super::*;
use crate::cli::text::compact_text;

pub(super) async fn run_enqueue(
    socket: &Path,
    config_path: Option<&Path>,
    rpc_timeout: Duration,
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
    let submission = match (args.dedup_key.as_ref(), args.submission) {
        (Some(_), CliSubmissionMode::Full) => Some(SubmissionOptions {
            mode: SubmissionMode::Full,
        }),
        _ => None,
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
        submission,
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
        caller_job_id: inherited_caller_job_id(),
        caller_job_token: inherited_caller_job_token(),
        gh_trigger_actor: None,
        gh_self_actor: None,
        gh_origin: None,
        task_uuid: None,
        related_trigger: args.related_trigger,
        wait: args.wait,
    };
    submit_payload(
        socket,
        config_path,
        rpc_timeout,
        "queue.enqueue",
        payload,
        args.wait,
    )
    .await
}

/// Say, at the point of degradation, that this admission's run membership was
/// not recorded.
///
/// The admission succeeded and its work is running — that is why the daemon
/// acknowledged it rather than refusing — but this node will be missing from
/// `--flow-run` windows until the ledger is repaired. Without this line the only
/// trace is a daemon journal entry the operator has to already know to grep for,
/// which means the person who caused the degradation is the one person who does
/// not learn about it.
pub(super) fn report_degraded_membership(result: &Value) -> Result<()> {
    let Some(degraded) = result
        .get("membershipDegraded")
        .filter(|value| value.is_object())
    else {
        return Ok(());
    };
    errln!(
        "warning: this node was admitted but its run membership was NOT recorded, so it \
         will be missing from `--flow-run` windows for run {} until the ledger is repaired \
         (task {}): {}. Resolution: {}.",
        compact_text(degraded["flowRunId"].as_str().unwrap_or("<unknown>")),
        compact_text(degraded["taskUuid"].as_str().unwrap_or("<unknown>")),
        compact_text(degraded["reason"].as_str().unwrap_or("<unknown>")),
        compact_text(
            degraded["resolution"]
                .as_str()
                .unwrap_or("repair-flow-membership-ledger")
        ),
    );
    Ok(())
}

pub(super) async fn submit_payload(
    socket: &Path,
    config_path: Option<&Path>,
    rearm_window: Duration,
    method: &str,
    payload: EnqueuePayload,
    wait: bool,
) -> Result<()> {
    let client = connect_rpc(socket, config_path).await?;
    let result = client
        .call(method, Some(serde_json::to_value(payload)?))
        .await?;
    report_degraded_membership(&result)?;
    if !wait {
        outln!("{}", serde_json::to_string(&result)?);
        return Ok(());
    }
    if let Some(verdict) = result.get("verdict").and_then(Value::as_str) {
        outln!("{}", serde_json::to_string(&result)?);
        let code = verdict_exit_code(verdict);
        if code != 0 {
            return Err(anyhow::Error::new(ExitFailure {
                code,
                message: format!("job finished with verdict {verdict}"),
            }));
        }
        return Ok(());
    }
    let task_uuid = result
        .get("task_uuid")
        .and_then(Value::as_str)
        .filter(|task_uuid| !task_uuid.is_empty())
        .ok_or_else(|| invalid(format!("{method} returned no task_uuid for --wait")))?;
    let waited = await_job_with_rearm(client, socket, task_uuid, rearm_window).await?;
    outln!("{}", serde_json::to_string(&waited)?);
    let code = waited_exit_code(&waited);
    if code == 0 {
        Ok(())
    } else {
        Err(anyhow::Error::new(ExitFailure {
            code,
            message: "waited job returned a non-zero verdict".to_owned(),
        }))
    }
}
