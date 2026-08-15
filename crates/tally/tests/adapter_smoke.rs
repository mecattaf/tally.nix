use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;
use tally_core::adapters::{
    AdapterConfig, AdapterLaunchConfig, ScrapeCapture, ScrapeMode, ScrapeStream,
};
use tally_core::config::{
    CoResidencyPredicate, Config, JournaldConfig, PoolConfig, PoolPredicate, ResourceKind,
};
use tally_core::daemon::{
    Daemon, DaemonError, DaemonPaths, DaemonSettings, DEFAULT_MAX_CONNECTIONS,
};
use tally_core::evidence::RetryPolicy;
use tally_core::executor::{
    ExecutionPaths, Executor, ExecutorError, LocalUnitFact, LocalUnitProbe, UnitLimits,
};
use tally_core::recovery::RecoveryPolicy;
use tally_core::witness::read_verified_records;
use tokio::process::Command;
use tokio::sync::watch;
use tokio::task::{JoinHandle, LocalSet};

#[path = "support/configured_tally.rs"]
mod configured_tally;
#[path = "support/shell_program.rs"]
mod shell_program;

const PRE_OUTPUT_FAILURE: &str =
    "Not inside a trusted directory and --skip-git-repo-check was not specified.";
const HEALTHY_ADAPTER_NOISE: &str = "Reading additional input from stdin...";

struct AbsentUnitProbe;

impl LocalUnitProbe for AbsentUnitProbe {
    fn inspect(&self, unit: &str, _paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        Ok(LocalUnitFact::absent(unit))
    }
}

struct RunningDaemon {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), DaemonError>>,
}

impl RunningDaemon {
    async fn stop(self) {
        self.shutdown.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("daemon shutdown timed out")
            .expect("daemon task panicked")
            .expect("daemon shutdown failed");
    }
}

fn daemon_paths(root: &Path) -> DaemonPaths {
    DaemonPaths {
        socket: root.join("run/tally.sock"),
        state_dir: root.join("state"),
        data_dir: root.join("data"),
    }
}

fn smoke_config(root: &Path) -> Config {
    let structured = root.join("structured-adapter");
    shell_program::install(
        &structured,
        concat!(
            "#!/bin/sh\n",
            "printf '%s\\n' 'TALLY_SESSION=smoke-session' 'TALLY_FINAL_MESSAGE=ok'\n",
            "printf '%s\\n' 'Reading additional input from stdin...' >&2\n",
        ),
    );
    let failing = root.join("pre-output-failure-adapter");
    shell_program::install(
        &failing,
        format!("#!/bin/sh\nprintf '%s\\n' '{PRE_OUTPUT_FAILURE}' >&2\nexit 1\n"),
    );
    let capture = |pattern: &str| ScrapeCapture {
        stream: ScrapeStream::Stdout,
        mode: ScrapeMode::Regex,
        pattern: pattern.to_owned(),
        counter_scope: None,
        fields: Default::default(),
    };
    Config {
        pools: BTreeMap::from([(
            "stock".to_owned(),
            PoolConfig {
                resource: Some(ResourceKind::BuildSlot),
                capacity: 1,
                predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                ..PoolConfig::default()
            },
        )]),
        adapters: BTreeMap::from([
            ("shell".to_owned(), AdapterConfig::default()),
            (
                "structured".to_owned(),
                AdapterConfig {
                    argv: vec![structured.display().to_string()],
                    scrape: BTreeMap::from([
                        ("sessionRef".to_owned(), capture("^TALLY_SESSION=(.*)$")),
                        (
                            "finalMessage".to_owned(),
                            capture("^TALLY_FINAL_MESSAGE=(.*)$"),
                        ),
                    ]),
                    ..AdapterConfig::default()
                },
            ),
            (
                "pre-output-failure".to_owned(),
                AdapterConfig {
                    argv: vec![failing.display().to_string()],
                    ..AdapterConfig::default()
                },
            ),
            (
                "committing".to_owned(),
                policy_adapter(root, "committing-adapter", COMMITTING_AGENT),
            ),
            (
                "writing-only".to_owned(),
                policy_adapter(root, "writing-only-adapter", WRITING_ONLY_AGENT),
            ),
        ]),
        journald: JournaldConfig { native: false },
        ..Config::default()
    }
}

/// Asserts the launch argv it actually received, then does what a spec-build
/// implementation node must do: write, stage, and commit.
const COMMITTING_AGENT: &str = concat!(
    "#!/bin/sh\n",
    "set -eu\n",
    "if [ \"$1\" != '-c' ] || [ \"$2\" != 'approval_policy=\"never\"' ] \\\n",
    "  || [ \"$3\" != '--sandbox' ] || [ \"$4\" != 'full' ] || [ \"$5\" != '--' ]; then\n",
    "  printf 'unexpected launch argv: %s\\n' \"$*\" >&2\n",
    "  exit 3\n",
    "fi\n",
    "printf 'ok\\n' > tally-commit-probe.txt\n",
    "git add --all\n",
    "git commit --quiet --message 'tally commit probe'\n",
    "printf 'done\\n'\n",
);

/// The exact shape of the shipped defect: the agent writes its work correctly
/// and cannot reach git metadata to publish it.
const WRITING_ONLY_AGENT: &str = concat!(
    "#!/bin/sh\n",
    "set -eu\n",
    "printf 'ok\\n' > tally-commit-probe.txt\n",
    "printf 'done\\n'\n",
);

fn policy_adapter(root: &Path, file: &str, body: &str) -> AdapterConfig {
    let program = root.join(file);
    shell_program::install(&program, body);
    AdapterConfig {
        argv: vec![program.display().to_string(), "--".to_owned()],
        launch: AdapterLaunchConfig {
            approval_policies: BTreeMap::from([(
                "never".to_owned(),
                vec!["-c".to_owned(), "approval_policy=\"never\"".to_owned()],
            )]),
            sandbox_policies: BTreeMap::from([
                (
                    "full".to_owned(),
                    vec!["--sandbox".to_owned(), "full".to_owned()],
                ),
                (
                    "confined".to_owned(),
                    vec!["--sandbox".to_owned(), "confined".to_owned()],
                ),
            ]),
            commit_capable_sandbox_policies: BTreeSet::from(["full".to_owned()]),
            ..AdapterLaunchConfig::default()
        },
        ..AdapterConfig::default()
    }
}

fn settings() -> DaemonSettings {
    DaemonSettings {
        unit_limits: UnitLimits {
            cpu_weight: Some(100),
            memory_max_bytes: Some(64 * 1024 * 1024),
        },
        yield_grace: Duration::from_secs(1),
        recovery_policy: RecoveryPolicy {
            retry: RetryPolicy {
                auto_pool_return: false,
                auto_resource_return: false,
                auto_bounded_requeue: false,
            },
            max_attempts: 1,
        },
        max_connections: DEFAULT_MAX_CONNECTIONS,
    }
}

async fn start_daemon(paths: &DaemonPaths, config: Config) -> RunningDaemon {
    let recorder = configured_tally::install(&paths.state_dir.join("configured-tally"));
    let executor = Executor::new(&paths.state_dir, recorder)
        .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
        .with_direct_fallback()
        .with_unit_probe(AbsentUnitProbe);
    let daemon = Daemon::open_with_executor(config, paths.clone(), settings(), executor)
        .await
        .unwrap();
    let (shutdown, receiver) = watch::channel(false);
    let task = tokio::task::spawn_local(daemon.run_until(receiver));
    RunningDaemon { shutdown, task }
}

async fn run_tally(config: &Path, socket: &Path, args: &[&str]) -> std::process::Output {
    // The commit probe defaults its repository to the state directory, so the
    // suite pins XDG_STATE_HOME rather than seeding repositories into whatever
    // home the developer or the gate happens to run under.
    let state_home = config.parent().unwrap().join("xdg-state");
    Command::new(env!("CARGO_BIN_EXE_tally"))
        .arg("--config")
        .arg(config)
        .arg("--socket")
        .arg(socket)
        .args(args)
        .env("XDG_STATE_HOME", &state_home)
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .output()
        .await
        .unwrap()
}

fn parse_stdout(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON stdout ({error}):\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[tokio::test(flavor = "current_thread")]
async fn commit_probe_asserts_a_publishable_commit_under_the_named_policies() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = daemon_paths(temp.path());
            let config = smoke_config(temp.path());
            let config_path = temp.path().join("config.json");
            std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let daemon = start_daemon(&paths, config).await;

            let probe_argv = |adapter: &'static str| {
                vec![
                    "adapter",
                    "smoke",
                    adapter,
                    "--pool",
                    "stock",
                    "--approval-policy",
                    "never",
                    "--sandbox",
                    "full",
                    "--assert-commit",
                ]
            };

            // An adapter that commits under these policies passes, and the
            // fixture only reaches its commit if the flags it was launched with
            // are the ones the operator named.
            let committing =
                run_tally(&config_path, &paths.socket, &probe_argv("committing")).await;
            assert_eq!(
                committing.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&committing.stderr)
            );
            let committing = parse_stdout(&committing);
            let probe = &committing["commitProbe"];
            assert_eq!(probe["status"], "verified");
            assert_eq!(probe["commits"], 1);
            assert_ne!(probe["headRev"], probe["baseRev"]);
            assert_eq!(probe["worktreeStatus"].as_array().unwrap().len(), 0);
            // The probe repository lives under the state directory, never under
            // the system temporary directory: a hardened adapter's unit gets a
            // private /tmp and could not chdir into one there, and an agent
            // sandbox may treat $TMPDIR as writable by default.
            let repository = PathBuf::from(probe["repository"].as_str().unwrap());
            assert!(
                repository.starts_with(temp.path().join("xdg-state").join("tally/adapter-smoke")),
                "{}",
                repository.display()
            );
            // A verified probe leaves nothing behind.
            assert!(!repository.exists());

            // An adapter that writes its work and cannot commit is the shipped
            // defect, and it fails the probe rather than passing the smoke.
            let writing_only =
                run_tally(&config_path, &paths.socket, &probe_argv("writing-only")).await;
            let stderr = String::from_utf8_lossy(&writing_only.stderr).into_owned();
            assert_eq!(writing_only.status.code(), Some(1), "{stderr}");
            assert!(stderr.contains("left no publishable commit"), "{stderr}");
            let writing_only = parse_stdout(&writing_only);
            let probe = &writing_only["commitProbe"];
            assert_eq!(probe["status"], "no-commit");
            assert_eq!(probe["commits"], 0);
            assert_eq!(probe["headRev"], probe["baseRev"]);
            // A failed probe is retained as the evidence, with the work in it.
            let retained = PathBuf::from(probe["repository"].as_str().unwrap());
            assert_eq!(
                std::fs::read_to_string(retained.join("tally-commit-probe.txt")).unwrap(),
                "ok\n"
            );
            std::fs::remove_dir_all(&retained).unwrap();

            // A policy name the adapter never declared would render no argv at
            // all, so it is refused before any job is admitted.
            let undeclared = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "committing",
                    "--pool",
                    "stock",
                    "--sandbox",
                    "danger-full-access",
                ],
            )
            .await;
            let stderr = String::from_utf8_lossy(&undeclared.stderr).into_owned();
            assert_eq!(undeclared.status.code(), Some(2), "{stderr}");
            assert!(
                stderr.contains("declares no sandbox policy \"danger-full-access\""),
                "{stderr}"
            );
            assert!(stderr.contains("confined, full"), "{stderr}");

            daemon.stop().await;
        })
        .await;
}

/// The probe repository is seeded only after the enqueue RPC has a connection,
/// so a failure that says nothing about the adapter — here, a daemon that is not
/// listening — leaves no git repository behind. Nothing but `tally gc` knows the
/// `adapter-smoke/probe-*` prefix, so an unreaped one is an operator's problem
/// for as long as they never notice it.
#[tokio::test(flavor = "current_thread")]
async fn an_unreachable_daemon_seeds_no_commit_probe_repository() {
    let temp = tempfile::tempdir().unwrap();
    let config = smoke_config(temp.path());
    let config_path = temp.path().join("config.json");
    std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let unreachable = run_tally(
        &config_path,
        &temp.path().join("run/nothing-is-listening.sock"),
        &[
            "adapter",
            "smoke",
            "committing",
            "--pool",
            "stock",
            "--approval-policy",
            "never",
            "--sandbox",
            "full",
            "--assert-commit",
        ],
    )
    .await;
    let stderr = String::from_utf8_lossy(&unreachable.stderr).into_owned();
    assert_eq!(unreachable.status.code(), Some(3), "{stderr}");
    let probe_root = temp.path().join("xdg-state/tally/adapter-smoke");
    assert!(
        !probe_root.exists(),
        "an unreachable daemon left {} behind",
        probe_root.display()
    );
}

/// Retaining a probe repository on failure is deliberate, but retention that
/// never names the path is not evidence an operator can collect. Every failure
/// after the seed names it, including the ones that have nothing to do with the
/// commit assertion.
#[tokio::test(flavor = "current_thread")]
async fn a_failure_unrelated_to_the_commit_assertion_still_names_the_retained_repository() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = daemon_paths(temp.path());
            let config = smoke_config(temp.path());
            let config_path = temp.path().join("config.json");
            std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let daemon = start_daemon(&paths, config).await;

            // This adapter fails before it produces any output, so the run never
            // reaches the commit evaluation at all.
            let failed = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "pre-output-failure",
                    "--pool",
                    "stock",
                    "--assert-commit",
                ],
            )
            .await;
            let stderr = String::from_utf8_lossy(&failed.stderr).into_owned();
            assert_eq!(failed.status.code(), Some(1), "{stderr}");
            assert!(stderr.contains("finished with verdict failed"), "{stderr}");
            assert!(
                stderr.contains("commit probe repository retained at"),
                "{stderr}"
            );

            let reported = parse_stdout(&failed);
            let retained = PathBuf::from(reported["commitProbe"]["repository"].as_str().unwrap());
            assert_eq!(reported["commitProbe"]["status"], "not-checked");
            assert!(
                stderr.contains(retained.to_str().unwrap()),
                "the retained path in the message is the one on disk: {stderr}"
            );
            assert!(retained.join(".git").is_dir(), "{}", retained.display());
            std::fs::remove_dir_all(&retained).unwrap();

            daemon.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn smoke_runs_real_jobs_parses_declared_captures_and_surfaces_pre_output_stderr() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = daemon_paths(temp.path());
            let config = smoke_config(temp.path());
            let config_path = temp.path().join("config.json");
            std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let daemon = start_daemon(&paths, config).await;

            let shell = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "shell",
                    "--cwd",
                    temp.path().to_str().unwrap(),
                ],
            )
            .await;
            assert_eq!(shell.status.code(), Some(0));
            let shell = parse_stdout(&shell);
            assert_eq!(shell["diagnostic"], "adapter-smoke");
            assert_eq!(shell["pool"], "stock");
            assert_eq!(shell["verdict"], "pass");
            assert_eq!(shell["captureStatus"], "not-declared");

            let structured = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "structured",
                    "--pool",
                    "stock",
                    "--cwd",
                    temp.path().to_str().unwrap(),
                ],
            )
            .await;
            assert_eq!(
                structured.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&structured.stderr)
            );
            let structured = parse_stdout(&structured);
            assert_eq!(structured["captureStatus"], "verified");
            assert_eq!(structured["captures"]["sessionRef"], "smoke-session");
            assert_eq!(structured["captures"]["finalMessage"], "ok");
            let structured_task_uuid = structured["taskUuid"].as_str().unwrap();
            let structured_raw_stderr = paths
                .state_dir
                .join("capture")
                .join(format!("{structured_task_uuid}.adapter.err"));
            let structured_failure_stderr = paths
                .state_dir
                .join("capture")
                .join(format!("{structured_task_uuid}.err"));
            assert_eq!(
                std::fs::read_to_string(structured_raw_stderr).unwrap(),
                format!("{HEALTHY_ADAPTER_NOISE}\n")
            );
            assert!(!structured_failure_stderr.exists());

            let failure = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "pre-output-failure",
                    "--pool",
                    "stock",
                    "--cwd",
                    temp.path().to_str().unwrap(),
                ],
            )
            .await;
            assert_eq!(failure.status.code(), Some(1));
            let failure_json = parse_stdout(&failure);
            assert_eq!(failure_json["verdict"], "failed");
            assert_eq!(failure_json["captureStatus"], "not-checked");
            let failure_stderr = String::from_utf8_lossy(&failure.stderr);
            assert!(
                failure_stderr.contains("captured stderr:"),
                "{failure_stderr}"
            );
            assert!(
                failure_stderr.contains(PRE_OUTPUT_FAILURE),
                "{failure_stderr}"
            );

            let task_uuid = failure_json["taskUuid"].as_str().unwrap();
            let raw_stderr = paths
                .state_dir
                .join("capture")
                .join(format!("{task_uuid}.adapter.err"));
            let persisted_stderr = paths
                .state_dir
                .join("capture")
                .join(format!("{task_uuid}.err"));
            for capture in [raw_stderr, persisted_stderr] {
                assert_eq!(
                    std::fs::read_to_string(capture).unwrap(),
                    format!("{PRE_OUTPUT_FAILURE}\n")
                );
            }

            let log = run_tally(
                &config_path,
                &paths.socket,
                &["query", "log", "--task", task_uuid, "--json"],
            )
            .await;
            assert_eq!(
                log.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&log.stderr)
            );
            let log = parse_stdout(&log);
            let failed = log["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["event"] == "failed")
                .unwrap();
            assert_eq!(failed["stderrTail"], format!("{PRE_OUTPUT_FAILURE}\n"));
            assert_eq!(failed["stderrTruncated"], false);

            let reconstructed = run_tally(
                &config_path,
                &paths.socket,
                &["queue", "await-job", task_uuid],
            )
            .await;
            assert_eq!(reconstructed.status.code(), Some(0));
            assert_eq!(
                parse_stdout(&reconstructed)["stderr_excerpt"],
                PRE_OUTPUT_FAILURE.to_owned() + "\n"
            );

            daemon.stop().await;
            let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
            assert!(report.ok);
            assert_eq!(records.len(), 3);
            for record in records {
                let marker = record.evidence_class.as_ref().unwrap();
                assert_eq!(marker["kind"], "adapter-smoke");
                assert!(marker["label"]
                    .as_str()
                    .unwrap()
                    .starts_with("adapter-smoke:"));
            }
        })
        .await;
}

/// The producer of the probe repositories and the sweep that reaps them must
/// name one place. `--probe-root` names a directory directly; `--state-dir`
/// names the directory the *default* probe root derives from, which is the same
/// derivation `tally gc --state-dir` walks. Without it the CLI resolves the XDG
/// state directory, which on a NixOS deployment is not the module's `stateDir`
/// (`/var/lib/tally/state`) that the retention timer passes to `tally gc` — so
/// every retained probe sat in a directory the sweep was never pointed at.
#[tokio::test(flavor = "current_thread")]
async fn the_smoke_probe_root_and_the_gc_sweep_root_agree_under_one_state_dir() {
    LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = daemon_paths(temp.path());
            let config = smoke_config(temp.path());
            let config_path = temp.path().join("config.json");
            std::fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let daemon = start_daemon(&paths, config).await;

            // This adapter fails, so the probe repository is retained: exactly
            // the population the sweep exists for.
            let failed = run_tally(
                &config_path,
                &paths.socket,
                &[
                    "adapter",
                    "smoke",
                    "pre-output-failure",
                    "--pool",
                    "stock",
                    "--assert-commit",
                    "--state-dir",
                    paths.state_dir.to_str().unwrap(),
                ],
            )
            .await;
            let stderr = String::from_utf8_lossy(&failed.stderr).into_owned();
            assert_eq!(failed.status.code(), Some(1), "{stderr}");
            let retained = PathBuf::from(
                parse_stdout(&failed)["commitProbe"]["repository"]
                    .as_str()
                    .unwrap(),
            );
            assert!(
                retained.starts_with(paths.state_dir.join("adapter-smoke")),
                "{}",
                retained.display()
            );
            // And explicitly *not* the XDG state directory the CLI resolves
            // when it is not told which state directory to use.
            assert!(
                !retained.starts_with(temp.path().join("xdg-state")),
                "{}",
                retained.display()
            );
            daemon.stop().await;

            // Age it past the horizon the shipped retention timer uses. A
            // directory cannot be opened for writing, so the read handle
            // carries the timestamps.
            let aged = SystemTime::now() - Duration::from_secs(60 * 60 * 24 * 31);
            std::fs::File::open(&retained)
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new()
                        .set_accessed(aged)
                        .set_modified(aged),
                )
                .unwrap();

            let gc_argv = |dry: bool| {
                let mut argv = vec![
                    "gc".to_owned(),
                    "--horizon".to_owned(),
                    "30d".to_owned(),
                    "--capture-archive-horizon".to_owned(),
                    "30d".to_owned(),
                    "--data-dir".to_owned(),
                    paths.data_dir.display().to_string(),
                    "--state-dir".to_owned(),
                    paths.state_dir.display().to_string(),
                ];
                if dry {
                    argv.push("--dry-run".to_owned());
                }
                argv
            };
            fn as_args(argv: &[String]) -> Vec<&str> {
                argv.iter().map(String::as_str).collect()
            }

            let dry = gc_argv(true);
            let dry = run_tally(&config_path, &paths.socket, &as_args(&dry)).await;
            assert_eq!(
                dry.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&dry.stderr)
            );
            let dry = parse_stdout(&dry);
            assert_eq!(dry["adapterProbesExamined"], 1);
            assert_eq!(dry["adapterProbesPruned"], 1);
            assert!(retained.exists(), "a dry run must not remove anything");

            let swept = gc_argv(false);
            let swept = run_tally(&config_path, &paths.socket, &as_args(&swept)).await;
            assert_eq!(
                swept.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&swept.stderr)
            );
            let swept = parse_stdout(&swept);
            assert_eq!(swept["adapterProbesExamined"], 1);
            assert_eq!(swept["adapterProbesPruned"], 1);
            assert!(
                !retained.exists(),
                "the gc sweep did not reach the probe root the smoke used: {}",
                retained.display()
            );
        })
        .await;
}
