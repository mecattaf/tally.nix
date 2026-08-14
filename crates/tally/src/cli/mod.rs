mod adapter;
mod args;
mod campaign;
mod commit_lint;
mod daemon;
pub(crate) mod enqueue;
mod exit;
mod flow;
mod out;
mod queue;
mod reader_state;
mod rebuild;
mod text;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};
use tally_client::{
    await_job_with_rearm, default_config_path, resolve_max_frame_bytes, RpcClient, WireErrorCode,
    WireIoError,
};
use tally_core::completion::{AcceptancePolicy, GateManifestSpec};
use tally_core::config::Priority;
use tally_core::daemon::{Daemon, DaemonPaths, DaemonSettings, DEFAULT_MAX_CONNECTIONS};
use tally_core::durable_view::{
    durable_run_view, rebuild_run_view, verify_rebuild_matches_live, DurableViewError,
};
use tally_core::evidence::RetryPolicy;
use tally_core::exec_attestation::{
    compare as compare_witness_attestations, read_verified_exec_attestations, run_exec,
    ExecRunRequest, EXEC_ATTESTATION_LEDGER,
};
use tally_core::executor::{
    persist_exit_record_from_env, serve_remote_executor_stdio, ExecutionPaths, Executor, UnitLimits,
};
use tally_core::producers::{record_producer_runtime, ProducerEngine, ProducerObservation};
use tally_core::provenance::Orchestration;
use tally_core::query_v2::{ObservabilityError, RunView};
use tally_core::recovery::RecoveryPolicy;
use tally_core::taskdb::{EnqueueSource, RelatedTrigger, WorkspaceMetadata};
use tally_core::wire::{EnqueuePayload, SubmissionMode, SubmissionOptions};
use tally_core::witness::{
    append_attestation, read_verified_attestations, read_verified_records, GENESIS_PREV_HASH,
};
use tally_core::{
    adapters::{provisions_gate_manifest, AdapterEngine, AdapterJobOptions, ScrapeResult},
    Config,
};
use tally_flow::{
    check_script, load_catalog, render_script, run_script, CheckOptions, FlowError, LifecycleSink,
    RunOptions, SourceLocation,
};

use crate::flow_live::{LiveFlowClient, RunnerIdentity};

const DEFAULT_RPC_TIMEOUT_SEC: u64 = 60;
const RPC_TIMEOUT_ENV: &str = "TALLY_RPC_TIMEOUT_SEC";
/// How long `flow run` waits for an advisory finalMessage projection after a
/// node's exit evidence passes (#432). Milliseconds, so a test can widen the
/// window without touching seconds. Unset keeps the 10 s default. It is
/// captured before the inherited-environment sanitize removes `TALLY_*`, the
/// same way `TALLY_RPC_TIMEOUT_SEC` is.
const RESULT_PROJECTION_TIMEOUT_ENV: &str = "TALLY_RESULT_PROJECTION_TIMEOUT_MS";
use adapter::*;
use args::*;
use campaign::*;
use commit_lint::*;
use daemon::*;
use enqueue::*;
use exit::*;
use flow::*;
use out::{errln, outln};
use queue::*;
use reader_state::*;
use rebuild::*;

pub(crate) fn main() {
    let mut args = std::env::args_os().collect::<Vec<_>>();
    let invoked_as_tallyd =
        args.first().and_then(|arg| Path::new(arg).file_name()) == Some(OsStr::new("tallyd"));
    if invoked_as_tallyd && args.len() == 1 {
        args.extend(["daemon".into(), "run".into()]);
    }
    let helper_mode = args.get(1).is_some_and(|argument| {
        matches!(
            argument.to_str(),
            Some("__record-unit-exit" | "__remote-executor")
        )
    });
    let opts = Opts::parse_from(args);
    let environment = InvocationEnvironment::capture(&opts);
    if environment.flow_runner.is_some() {
        sanitize_inherited_tally_environment();
    }
    // Flow evaluation runs on `spawn_blocking`, and Boa recursively
    // instantiates a roughly 100 KiB spec-build script. Tokio's 2 MiB default
    // is already below the debug-engine requirement pinned by the direct flow
    // tests; give runtime-created blocking threads the same bounded headroom.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .thread_stack_size(4 * 1024 * 1024)
        .enable_all()
        .build()
        .expect("tally tokio runtime must build");
    match runtime.block_on(execute(opts, environment)) {
        Ok(()) => {}
        Err(error) => {
            if !helper_mode && !error.to_string().is_empty() {
                // The last-resort printer cannot propagate anything: this is
                // already the error path and `main` returns nothing. A closed
                // stderr is dropped rather than panicked on, which is the same
                // silence the stdout path takes.
                let _ = out::write_error_line(format_args!("tally: {error:#}"));
            }
            std::process::exit(error_exit_code(&error));
        }
    }
}

async fn execute(opts: Opts, environment: InvocationEnvironment) -> Result<()> {
    if matches!(opts.mode, Some(Mode::CheckConfig)) {
        let path = opts
            .config
            .context("--mode check-config requires --config PATH")?;
        Config::from_path(&path)?;
        outln!("{}", serde_json::to_string(&configuration_contract())?);
        return Ok(());
    }

    let rpc_timeout =
        resolve_rpc_timeout(opts.rpc_timeout_sec, environment.rpc_timeout.as_deref())?;
    let socket = opts.socket.unwrap_or_else(default_socket_path);
    match opts.command {
        Some(Command::RecordUnitExit(args)) => {
            persist_exit_record_from_env(&args.record, &args.unit, &args.systemctl)?;
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
                        failure_stderr: PathBuf::from("unused"),
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
            outln!(
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
        Some(Command::AdapterSmokeShell) => {
            outln!("ok");
            Ok(())
        }
        Some(Command::AdapterSmokeCommit) => run_adapter_smoke_commit(),
        Some(Command::ProducerDispatch(args)) => {
            run_producer_dispatch(opts.config, &socket, args).await
        }
        Some(Command::Daemon {
            command: DaemonCommand::Run { mock: true, .. },
        }) => {
            outln!("tally mock daemon ready");
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
        Some(Command::Daemon {
            command: DaemonCommand::Drain,
        }) => {
            let drained = print_rpc(
                &socket,
                opts.config.as_deref(),
                rpc_timeout,
                "queue.drain",
                Some(json!({})),
            )
            .await;
            // A periodic drain that finds no daemon is not an error (#411).
            // `tally-drain.timer` fires every five seconds and a daemon restart
            // takes longer than that, so every activation that restarts
            // `tally-daemon` has a good chance of catching a drain mid-flight;
            // the exit 3 that produced surfaced as a genuine per-user unit
            // failure, which is exactly the shape the fleet's journal watcher
            // exists to catch. Spending that alarm on a benign restart is the
            // cost being removed here.
            //
            // Narrow on purpose: only the socket-absent case and #427's
            // deadline-expired case are absorbed, and the line naming each is
            // still written, so an operator running the verb by hand still
            // learns which case it was. Every other drain failure -- including
            // a daemon that is listening and refuses -- keeps its exit code.
            match drained {
                Err(error) if is_daemon_absent(&error) => {
                    let _ = out::write_error_line(format_args!("tally: {error:#}"));
                    Ok(())
                }
                // #427: the daemon is present -- the connection was
                // established -- but too busy to answer `queue.drain` within
                // the deadline. The producer event files are durable on disk
                // and the next `tally-drain` tick picks them up, so nothing is
                // lost: the periodic drain records a retryable skip (systemd
                // success plus the warning line below) instead of a unit
                // failure the fleet watcher would report dozens of times a day.
                Err(error) if is_drain_deadline_exceeded(&error) => {
                    let _ = out::write_error_line(format_args!(
                        "tally: {error:#} (retryable skip: the event files remain on disk and \
                         the next drain picks them up)"
                    ));
                    Ok(())
                }
                other => other,
            }
        }
        Some(Command::Gc(args)) => run_gc(args),
        Some(Command::Queue {
            command: QueueCommand::Enqueue(args),
        }) => run_enqueue(&socket, opts.config.as_deref(), rpc_timeout, *args).await,
        Some(Command::Queue { command }) => {
            run_queue(&socket, opts.config.as_deref(), rpc_timeout, command).await
        }
        Some(Command::Adapter { command }) => {
            run_adapter(&socket, opts.config.as_deref(), rpc_timeout, command).await
        }
        Some(Command::Campaign { command }) => {
            run_campaign(&socket, opts.config.as_deref(), rpc_timeout, command).await
        }
        Some(Command::Rebuild(args)) => {
            run_rebuild(&socket, opts.config.as_deref(), rpc_timeout, args).await
        }
        Some(Command::LintHistory(args)) => run_lint_history(args),
        Some(Command::Witness { command }) => run_witness(command),
        Some(Command::History { command }) => run_history(command),
        Some(Command::Attest { command }) => run_attest(command),
        Some(Command::Lease { command }) => {
            run_lease(&socket, opts.config.as_deref(), rpc_timeout, command).await
        }
        Some(Command::Query { command }) => {
            run_query(&socket, opts.config.as_deref(), rpc_timeout, command).await
        }
        Some(Command::Flow { command }) => {
            run_flow(
                &socket,
                opts.config.as_deref(),
                rpc_timeout,
                command,
                environment.flow_runner.unwrap_or_default(),
                environment.result_projection_timeout,
            )
            .await
        }
        Some(Command::ReaderState { command }) => run_reader_state(command),
        None => {
            Opts::command().print_help().map_err(out::map_write_error)?;
            outln!();
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
    let max_frame_bytes = config.max_frame_bytes;
    let event: ProducerObservation = serde_json::from_str(&args.event)
        .context("--event must be a producer observation JSON object")?;
    let state_dir = args.state_dir.map_or_else(default_state_dir, Ok)?;
    // Not optional: the brief store belongs to the daemon data directory, and
    // defaulting it to the state directory recreated the split brief layout
    // #271 retired. Generated units always pass the flag; a direct call must
    // name it too.
    let data_dir = args.data_dir;
    let events_dir = state_dir.join("events");
    let engine = ProducerEngine::new(&config.producers, &events_dir, &data_dir);
    let expected_kind = match &event {
        ProducerObservation::Calendar => "calendar",
        ProducerObservation::EventsDir => "events-dir",
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
        errln!(
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
                    errln!(
                        "tally: producer runtime update for {producer_name:?} could not notify the daemon: {error}"
                    );
                }
            }
            Err(error) => {
                errln!(
                    "tally: producer runtime update for {producer_name:?} could not reach the daemon: {error}"
                );
            }
        }
    }
    let result = dispatched?;
    outln!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn run_history(command: HistoryCommand) -> Result<()> {
    match command {
        HistoryCommand::Compact(args) => {
            let state_dir = args.state_dir.map_or_else(default_state_dir, Ok)?;
            let data_dir = args.data_dir.map_or_else(default_data_dir, Ok)?;
            let outcome = tally_core::history::compact_lifecycle(
                &state_dir,
                &data_dir,
                args.keep_days,
                chrono::Utc::now(),
            )?;
            outln!("{}", serde_json::to_string_pretty(&outcome)?);
            Ok(())
        }
    }
}

fn run_witness(command: WitnessCommand) -> Result<()> {
    match command {
        WitnessCommand::Append { ledger, payload } => {
            let path = ledger.unwrap_or(default_data_dir()?.join("attestations.jsonl"));
            let payload = serde_json::from_str(&payload).context("--payload must be valid JSON")?;
            let record = append_attestation(&path, payload)?;
            outln!("{}", serde_json::to_string(&record)?);
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
                        outln!(
                            "verdict chain: ok ({} records, seq {:?}..{:?})",
                            verdict_report.records,
                            verdict_report.first_seq,
                            verdict_report.last_seq
                        );
                    } else {
                        outln!("verdict chain: invalid");
                        for problem in &verdict_report.problems {
                            outln!(
                                "line {} seq {:?} {:?}: {}",
                                problem.line,
                                problem.seq,
                                problem.kind,
                                problem.reason
                            );
                        }
                    }
                    outln!(
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
                        outln!(
                            "execution attestation chain {}: {} ({} records; {})",
                            path.display(),
                            if report.ok { "ok" } else { "invalid" },
                            report.records,
                            report.authentication
                        );
                        if let Some(problem) = &report.problem {
                            outln!("  {problem}");
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
                    outln!(
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
                    outln!("{}", serde_json::to_string(&report)?);
                }
                WitnessVerifyFormat::Text => {
                    for execution in &report.executions {
                        outln!(
                            "{} {} {:?}",
                            execution.witness_ref,
                            execution.execution_id,
                            execution.agreement
                        );
                        for diff in &execution.diffs {
                            outln!("  {diff}");
                        }
                    }
                    outln!(
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
    let state_dir = if args.skip_state_dir {
        None
    } else {
        Some(args.state_dir.map_or_else(default_state_dir, Ok)?)
    };
    if state_dir.as_deref().is_some_and(|dir| !dir.is_absolute()) {
        return Err(invalid("--state-dir must be absolute"));
    }
    let state_retention = tally_core::retention::StateRetentionPolicy::parse(
        &args.capture_archive_horizon,
        &args.events_done_horizon,
        &args.events_rejected_horizon,
        args.events_rejected_max_count,
    )?;
    let report = tally_core::retention::run_gc(
        tally_core::retention::GcRequest {
            data_dir: &data_dir,
            state_dir: state_dir.as_deref(),
            horizon_text: &args.horizon,
            state_retention,
            now: Utc::now(),
            dry_run: args.dry_run,
            collect: args.collect,
        },
        &tally_core::nix_store::NixStore::default(),
    )?;
    outln!("{}", serde_json::to_string(&report)?);
    Ok(())
}
