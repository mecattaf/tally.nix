use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tally_client::RpcClient;
use tally_core::adapters::{
    AdapterConfig, AdapterLaunchConfig, AdapterValueOverride, ScrapeCapture, ScrapeMode,
    ScrapeStream,
};
use tally_core::config::{
    CoResidencyPredicate, Config, JournaldConfig, PoolConfig, PoolPredicate, ResourceKind,
};
use tally_core::daemon::{
    Daemon, DaemonError, DaemonPaths, DaemonSettings, DEFAULT_MAX_CONNECTIONS,
};
use tally_core::evidence::RetryPolicy;
use tally_core::executor::{
    read_exit_record, ExecutionPaths, Executor, ExecutorError, LocalUnitFact, LocalUnitProbe,
    LocalUnitState, UnitLimits,
};
use tally_core::recovery::RecoveryPolicy;
use tally_core::taskdb::{read_acknowledged_events, EnqueueSource};
use tally_core::witness::{read_verified_attestations, read_verified_records};
use tally_flow::{BRIEF_SENTINEL, SUPERSESSION_DETAIL_FIELDS};
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

#[path = "support/configured_tally.rs"]
mod configured_tally;
#[path = "support/shell_program.rs"]
mod shell_program;
#[path = "support/timeout_scale.rs"]
mod timeout_scale;

use timeout_scale::{effective_scale, scaled, TIMEOUT_SCALE_ENV};

/// The wall-clock budget every polling wait in this suite gets. It is routed
/// through [`scaled`] like every `tokio::time::timeout` budget here, so widening
/// the gate on a loaded host widens the tight waits and not only the loose ones.
const POLL_BUDGET: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A scaled deadline plus the text that names the knob when it expires.
///
/// A fixed iteration count is not a budget: it drifts with however long each
/// probe's RPC took, so the same loop is a 10-second wait on an idle host and an
/// unbounded one under load. A deadline is honest about what it is waiting for,
/// and reports the multiplier that produced it.
fn poll_deadline() -> (tokio::time::Instant, String) {
    let budget = scaled(POLL_BUDGET);
    (
        tokio::time::Instant::now() + budget,
        format!(
            "within {budget:.1?} ({TIMEOUT_SCALE_ENV}={})",
            effective_scale()
        ),
    )
}

const CONCURRENT_RUN: &str = "00000000-0000-4000-8000-000000000501";
const KILLED_RUN: &str = "00000000-0000-4000-8000-000000000502";
const RESTARTED_RUN: &str = "00000000-0000-4000-8000-000000000503";
const DIVERGENT_RUN: &str = "00000000-0000-4000-8000-000000000504";
const DIVERGENT_SUCCESSOR_RUN: &str = "00000000-0000-4000-8000-000000000537";
const DRV_BUILD_RUN: &str = "00000000-0000-4000-8000-000000000505";
const DRV_SUBSTITUTE_RUN: &str = "00000000-0000-4000-8000-000000000506";
const STRUCTURED_REPLAY_RUN: &str = "00000000-0000-4000-8000-000000000507";
const UNTYPED_RESULT_RUN: &str = "00000000-0000-4000-8000-000000000508";
const CREDENTIAL_REPLAY_RUN: &str = "00000000-0000-4000-8000-000000000509";
const REPLAY_DIVERGENCE_RUN: &str = "00000000-0000-4000-8000-000000000538";
const AUTO_REQUEUE_RUN: &str = "00000000-0000-4000-8000-000000000510";
const CANCELLED_RUN: &str = "00000000-0000-4000-8000-000000000511";
const CAP_REPLAY_RUN: &str = "00000000-0000-4000-8000-000000000512";
const PARTIAL_FAILURE_RUN: &str = "00000000-0000-4000-8000-000000000513";
const REORDERED_RUN: &str = "00000000-0000-4000-8000-000000000514";
const CATALOG_PIN_RUN: &str = "00000000-0000-4000-8000-000000000515";
const REGEX_RESULT_RUN: &str = "00000000-0000-4000-8000-000000000516";
const SPEC_BUILD_RUN: &str = "00000000-0000-4000-8000-000000000517";
const SPEC_BUILD_RUN_2: &str = "00000000-0000-4000-8000-000000000518";
const SPEC_BUILD_RUN_3: &str = "00000000-0000-4000-8000-000000000519";
const SPEC_BUILD_RUN_4: &str = "00000000-0000-4000-8000-000000000520";
const SPEC_BUILD_RUN_5: &str = "00000000-0000-4000-8000-000000000521";
const SPEC_BUILD_DUPLICATE_GATE_RUN: &str = "00000000-0000-4000-8000-000000000522";
const SPEC_BUILD_RUN_6: &str = "00000000-0000-4000-8000-000000000523";
const SPEC_BUILD_RUN_7: &str = "00000000-0000-4000-8000-000000000524";
const SPEC_BUILD_RUN_8: &str = "00000000-0000-4000-8000-000000000525";
const SPEC_BUILD_RUN_9: &str = "00000000-0000-4000-8000-000000000526";
const SPEC_BUILD_ORPHAN_RUN: &str = "00000000-0000-4000-8000-000000000527";
const SPEC_BUILD_DEFERRED_RUN: &str = "00000000-0000-4000-8000-000000000528";
const SPEC_BUILD_ATTACHED_RUN: &str = "00000000-0000-4000-8000-000000000529";
const SPEC_BUILD_RUN_CHECKPOINT_STEER: &str = "00000000-0000-4000-8000-000000000530";
const SPEC_BUILD_RUN_RENAMED: &str = "00000000-0000-4000-8000-000000000531";
const SPEC_BUILD_RUN_MACHINERY: &str = "00000000-0000-4000-8000-000000000532";
const SPEC_BUILD_RUN_RECOVERED: &str = "00000000-0000-4000-8000-000000000533";
const SPEC_BUILD_RUN_HALTED: &str = "00000000-0000-4000-8000-000000000534";
const SPEC_BUILD_RUN_LAST_TASK: &str = "00000000-0000-4000-8000-000000000535";
const SPEC_BUILD_RUN_REGATE: &str = "00000000-0000-4000-8000-000000000538";
const SPEC_BUILD_RUN_COMPLETE: &str = "00000000-0000-4000-8000-000000000536";
const SPEC_BUILD_RED_PREFLIGHT_RUN: &str = "00000000-0000-4000-8000-000000000537";
const DRV_PATH: &str = "/nix/store/00000000000000000000000000000000-flow-fixture.drv";
const DRV_OUTPUT: &str = "/nix/store/11111111111111111111111111111111-flow-fixture";
static ENVIRONMENT_LOCK: Mutex<()> = Mutex::const_new(());

struct ExitFileProbe;

impl LocalUnitProbe for ExitFileProbe {
    fn inspect(&self, unit: &str, paths: &ExecutionPaths) -> Result<LocalUnitFact, ExecutorError> {
        if !paths.exit_record.exists() {
            return Ok(LocalUnitFact::absent(unit));
        }
        let record = read_exit_record(&paths.exit_record, unit)?;
        Ok(LocalUnitFact {
            unit: unit.to_owned(),
            loaded: false,
            state: LocalUnitState::Exited,
            invocation_id: Some(record.invocation_id.clone()),
            attempt: Some(record.attempt),
            lease_epoch: Some(record.lease_epoch),
            exit_record: Some(record),
        })
    }
}

struct RunningDaemon {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<Result<(), DaemonError>>,
}

struct PathGuard {
    original: Option<OsString>,
}

impl PathGuard {
    fn prepend(directory: &Path) -> Self {
        let original = std::env::var_os("PATH");
        let mut entries = vec![directory.to_path_buf()];
        if let Some(path) = &original {
            entries.extend(std::env::split_paths(path));
        }
        std::env::set_var("PATH", std::env::join_paths(entries).unwrap());
        Self { original }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

impl RunningDaemon {
    async fn stop(self) {
        self.shutdown.send(true).unwrap();
        tokio::time::timeout(scaled(Duration::from_secs(10)), self.task)
            .await
            .expect("daemon shutdown timed out")
            .expect("daemon task panicked")
            .expect("daemon shutdown failed");
    }
}

fn config() -> Config {
    let pool = |resource| PoolConfig {
        resource: Some(resource),
        capacity: 8,
        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
        ..PoolConfig::default()
    };
    Config {
        pools: BTreeMap::from([
            ("flow".to_owned(), pool(ResourceKind::CpuSlot)),
            ("build".to_owned(), pool(ResourceKind::BuildSlot)),
            ("alpha".to_owned(), pool(ResourceKind::BuildSlot)),
            ("beta".to_owned(), pool(ResourceKind::BuildSlot)),
        ]),
        adapters: BTreeMap::from([("shell".to_owned(), AdapterConfig::default())]),
        journald: JournaldConfig { native: false },
        ..Config::default()
    }
}

fn install_fake_nix(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, PathGuard) {
    let bin = root.join("fake-nix-bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = root.join("store-output-valid");
    let builds = root.join("nix-builds");
    let nix = bin.join("nix");
    shell_program::install(
        &nix,
        format!(
            concat!(
                "#!/bin/sh\n",
                "case \" $* \" in\n",
                "  *\" --dry-run \"*)\n",
                "    if [ -e '{}' ]; then printf '[]\\n'; ",
                "else printf 'this derivation will be built\\n' >&2; fi\n",
                "    exit 0\n",
                "    ;;\n",
                "  *\" --max-jobs 0 \"*)\n",
                "    test -e '{}'\n",
                "    ;;\n",
                "  *)\n",
                "    : > '{}'\n",
                "    printf 'build\\n' >> '{}'\n",
                "    printf '[]\\n'\n",
                "    ;;\n",
                "esac\n"
            ),
            marker.display(),
            marker.display(),
            marker.display(),
            builds.display(),
        ),
    );
    let nix_store = bin.join("nix-store");
    shell_program::install(
        &nix_store,
        format!(
            concat!(
                "#!/bin/sh\n",
                "case \"$1\" in\n",
                "  --check-validity) test -e '{}' ;;\n",
                "  --add-root) exit 0 ;;\n",
                "  *) exit 93 ;;\n",
                "esac\n"
            ),
            marker.display(),
        ),
    );
    let path_guard = PathGuard::prepend(&bin);
    (marker, builds, path_guard)
}

fn install_fake_systemd_run(root: &Path, state_dir: &Path) -> std::path::PathBuf {
    let program = root.join("fake-systemd-run");
    shell_program::install(
        &program,
        format!(
            concat!(
                "#!/bin/sh\n",
                "unit=\n",
                "attempt=\n",
                "lease_epoch=\n",
                "while [ \"$#\" -gt 0 ]; do\n",
                "  case \"$1\" in\n",
                "    --unit) unit=\"$2\"; shift 2 ;;\n",
                "    --setenv)\n",
                "      case \"$2\" in\n",
                "        TALLY_ATTEMPT=*) attempt=\"${{2#TALLY_ATTEMPT=}}\" ;;\n",
                "        TALLY_LEASE_EPOCH=*) lease_epoch=\"${{2#TALLY_LEASE_EPOCH=}}\" ;;\n",
                "      esac\n",
                "      shift 2\n",
                "      ;;\n",
                "    --) break ;;\n",
                "    *) shift ;;\n",
                "  esac\n",
                "done\n",
                "test -n \"$unit\" -a -n \"$attempt\" -a -n \"$lease_epoch\" || exit 90\n",
                "uuid=\"${{unit#${{unit%????????-????-????-????-????????????}}}}\"\n",
                "record='{}/unit-exit/'\"$uuid\"'.json'\n",
                "mkdir -p '{}/unit-exit'\n",
                "printf '{{\"schemaVersion\":2,\"unit\":\"%s.service\",\"invocationId\":\"fake-systemd-run\",\"attempt\":%s,\"leaseEpoch\":%s,\"serviceResult\":\"success\",\"exitCode\":\"exited\",\"exitStatus\":\"0\"}}' \"$unit\" \"$attempt\" \"$lease_epoch\" > \"$record\"\n",
            ),
            state_dir.display(),
            state_dir.display(),
        ),
    );
    program
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
            max_attempts: 2,
        },
        max_connections: DEFAULT_MAX_CONNECTIONS,
    }
}

fn paths(root: &Path) -> DaemonPaths {
    DaemonPaths {
        socket: root.join("run/tally.sock"),
        state_dir: root.join("state"),
        data_dir: root.join("data"),
    }
}

async fn start_daemon(paths: &DaemonPaths, config: Config) -> RunningDaemon {
    start_daemon_with_systemd_run(paths, config, paths.state_dir.join("absent-systemd-run")).await
}

async fn start_daemon_with_settings(
    paths: &DaemonPaths,
    config: Config,
    settings: DaemonSettings,
) -> RunningDaemon {
    start_daemon_with_systemd_run_and_settings(
        paths,
        config,
        paths.state_dir.join("absent-systemd-run"),
        settings,
    )
    .await
}

async fn start_daemon_with_systemd_run(
    paths: &DaemonPaths,
    config: Config,
    systemd_run: std::path::PathBuf,
) -> RunningDaemon {
    start_daemon_with_systemd_run_and_settings(paths, config, systemd_run, settings()).await
}

async fn start_daemon_with_systemd_run_and_settings(
    paths: &DaemonPaths,
    config: Config,
    systemd_run: std::path::PathBuf,
    settings: DaemonSettings,
) -> RunningDaemon {
    let recorder = configured_tally::install(&paths.state_dir.join("configured-tally"));
    let executor = Executor::new(&paths.state_dir, recorder)
        .with_systemd_run(systemd_run)
        .with_direct_fallback()
        .with_unit_probe(ExitFileProbe);
    let daemon = Daemon::open_with_executor(config, paths.clone(), settings, executor)
        .await
        .unwrap();
    let (shutdown, receiver) = watch::channel(false);
    let task = tokio::task::spawn_local(daemon.run_until(receiver));
    RunningDaemon { shutdown, task }
}

fn six_node_source() -> &'static str {
    r#"
export const meta = {
  name: "fs5-six-node",
  description: "live heterogeneous runner integration",
  pools: ["alpha", "beta"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 6
};

(async () => parallel([
  () => sh(["/bin/sh", "-c", "exit 0"], {
    pools: ["alpha"], priority: "high", evidence: ["exit:0"],
    env: { FLOW_KIND: "shell-a" }, label: "alpha-shell"
  }),
  () => sh(["/bin/sh", "-c", "test 2 -gt 1"], {
    pools: ["beta"], priority: "low", evidence: ["exit:0"], label: "beta-true"
  }),
  () => sh(["/bin/sh", "-c", "sleep 0.02"], {
    pools: ["alpha"], evidence: ["exit:0"], label: "alpha-delay"
  }),
  () => sh(["/bin/sh", "-c", "test \"$FLOW_KIND\" = shell-b"], {
    pools: ["beta"], evidence: ["exit:0"], env: { FLOW_KIND: "shell-b" },
    label: "beta-env"
  }),
  () => sh(["/bin/sh", "-c", ":"], {
    pools: ["alpha"], evidence: ["exit:0"], label: "alpha-true"
  }),
  () => sh(["/bin/sh", "-c", "exit 0"], {
    pools: ["beta"], priority: "medium", evidence: ["exit:0"], label: "beta-shell"
  })
]))()
"#
}

fn task_ref_one_node_source() -> &'static str {
    r#"
export const meta = {
  name: "task-ref-fake-systemd",
  description: "taskRef-qualified fake systemd execution",
  pools: ["alpha"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 1
};

(async () => sh(["/bin/sh", "-c", "exit 0"], {
  pools: ["alpha"], evidence: ["exit:0"], label: "task-ref-child",
  taskRef: "crm/t07"
}))()
"#
}

fn divergent_source() -> &'static str {
    r#"
export const meta = {
  name: "fs5-divergence",
  description: "live replay divergence",
  pools: ["alpha"],
  argsSchema: {
    type: "object",
    required: ["variant"],
    properties: { variant: { type: "string" } },
    additionalProperties: false
  },
  selectors: [],
  maxNodes: 1
};

(async () => sh(["/bin/sh", "-c", "exit 0", args.variant], {
  pools: ["alpha"],
  evidence: ["exit:0"],
  label: "variant-" + args.variant
}))()
"#
}

fn automatic_requeue_source(sentinel: &Path) -> String {
    let sentinel = serde_json::to_string(&sentinel.to_string_lossy()).unwrap();
    format!(
        r#"
export const meta = {{
  name: "automatic-requeue",
  description: "the bounded retry reaches attempt two",
  pools: ["alpha"],
  argsSchema: {{ type: "object", additionalProperties: false }},
  selectors: [],
  maxNodes: 1
}};

(async () => sh(["/bin/sh", "-c",
  "if test -e \"$1\"; then exit 0; else : > \"$1\"; sleep 30; fi",
  "requeue", {sentinel}
], {{
  pools: ["alpha"], evidence: ["exit:0"], runtimeMaxSec: 1,
  label: "bounded-requeue"
}}))()
"#
    )
}

fn cancellation_source() -> &'static str {
    r#"
export const meta = {
  name: "flow-cancellation",
  description: "flow-scoped force cancellation",
  pools: ["alpha"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 1
};

(async () => sh(["/bin/sh", "-c", "sleep 30"], {
  pools: ["alpha"], evidence: ["exit:0"], label: "cancel-me"
}))()
"#
}

fn cancelled_cap_replay_source() -> &'static str {
    r#"
export const meta = {
  name: "cancelled-cap-replay",
  description: "cancelled rows release their flow node slot",
  pools: ["alpha"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 2
};

(async () => {
  const cancelled = await sh(["/bin/sh", "-c", "sleep 30"], {
    pools: ["alpha"], evidence: ["exit:0"], label: "cancelled-slot",
    settle: true
  });
  const replacement = await sh(["/bin/sh", "-c", "exit 0"], {
    pools: ["alpha"], evidence: ["exit:0"], label: "replacement"
  });
  return [cancelled.verdict, replacement.verdict];
})()
"#
}

fn partial_parallel_failure_source() -> &'static str {
    r#"
export const meta = {
  name: "partial-parallel-failure",
  description: "all live branches become terminal when one fails",
  pools: ["alpha", "beta"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 2
};

(async () => parallel([
  () => sh(["/bin/sh", "-c", "exit 0"], {
    pools: ["alpha"], evidence: ["exit:0"], label: "passing-branch"
  }),
  () => sh(["/bin/sh", "-c", "exit 7"], {
    pools: ["beta"], evidence: ["exit:0"], label: "failing-branch"
  })
]))()
"#
}

fn reordered_parallel_source() -> &'static str {
    r#"
export const meta = {
  name: "reordered-parallel",
  description: "ordinal identity survives a changed dedup key",
  pools: ["alpha", "beta"],
  argsSchema: {
    type: "object",
    required: ["reverse"],
    properties: { reverse: { type: "boolean" } },
    additionalProperties: false
  },
  selectors: [],
  maxNodes: 2
};

const left = () => sh(["/bin/sh", "-c", "printf left"], {
  pools: ["alpha"], evidence: ["exit:0"], dedupKey: "parallel-left",
  label: "left"
});
const right = () => sh(["/bin/sh", "-c", "printf right"], {
  pools: ["beta"], evidence: ["exit:0"], dedupKey: "parallel-right",
  label: "right"
});

(async () => parallel(args.reverse ? [right, left] : [left, right]))()
"#
}

fn catalog_pin_source() -> &'static str {
    r#"
export const meta = {
  name: "catalog-pin",
  description: "catalog bytes are run identity even without selection",
  pools: ["alpha"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 1
};

(async () => sh(["/bin/sh", "-c", "exit 0"], {
  pools: ["alpha"], evidence: ["exit:0"], label: "fixed-work"
}))()
"#
}

fn drv_source() -> String {
    format!(
        r#"
export const meta = {{
  name: "drv-store-native",
  description: "build once and substitute from the Nix store",
  pools: [],
  argsSchema: {{ type: "object", additionalProperties: false }},
  selectors: [],
  maxNodes: 1
}};

(async () => drv({{
  drvPath: {DRV_PATH:?},
  outputs: [{{ name: "out", path: {DRV_OUTPUT:?} }}]
}}))()
"#
    )
}

fn structured_result_source() -> &'static str {
    r#"
export const meta = {
  name: "structured-result-replay",
  description: "live adapter result survives terminal acknowledgement and restart",
  pools: ["alpha"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 1
};

(async () => {
  const node = await job({
    argv: [
      "/bin/sh",
      "-c",
      "printf '%s\n' '{\"final_message\":\"{\\\"answer\\\":42}\"}'"
    ],
    adapter: "structured",
    pools: ["alpha"],
    evidence: ["exit:0"],
    resultSchema: {
      type: "object",
      required: ["answer"],
      properties: { answer: { const: 42 } },
      additionalProperties: false
    },
    label: "structured-result"
  });
  return node.result;
})()
"#
}

fn untyped_result_source() -> &'static str {
    r#"
export const meta = {
  name: "untyped-result",
  description: "live adapter result is observed without a result schema",
  pools: ["alpha"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 1
};

(async () => {
  const node = await job({
    argv: [
      "/bin/sh",
      "-c",
      "printf '%s\n' '{\"final_message\":\"{\\\"answer\\\":42}\"}'"
    ],
    adapter: "structured",
    pools: ["alpha"],
    evidence: ["exit:0"],
    label: "untyped-result"
  });
  return node.result;
})()
"#
}

fn regex_result_source() -> &'static str {
    r#"
export const meta = {
  name: "regex-result",
  description: "live regex adapter result is projected before restart",
  pools: ["alpha"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 1
};

(async () => {
  const node = await job({
    argv: [
      "/bin/sh",
      "-c",
      "printf '%s\\n' 'TALLY_FINAL_MESSAGE={\"ok\":true,\"n\":3}'"
    ],
    adapter: "ocr-driver",
    pools: ["alpha"],
    evidence: ["exit:0"],
    resultSchema: {
      type: "object",
      required: ["ok", "n"],
      properties: {
        ok: { const: true },
        n: { const: 3 }
      },
      additionalProperties: false
    },
    label: "regex-result"
  });
  return node.result;
})()
"#
}

fn runner(
    config_path: &Path,
    socket: &Path,
    script: &Path,
    flow_run_id: &str,
    args: &str,
    max_nodes: u32,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
    command
        .arg("--config")
        .arg(config_path)
        .arg("--socket")
        .arg(socket)
        .args(["flow", "run"])
        .arg(script)
        .arg("--args")
        .arg(args)
        .arg("--max-nodes")
        .arg(max_nodes.to_string())
        .arg("--flow-run-id")
        .arg(flow_run_id)
        .env_remove("TALLY_TASK_UUID")
        .env_remove("TALLY_JOB_ID")
        .env_remove("TALLY_JOB_TOKEN")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

fn repository_fixture(path: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

/// Resolve the actual Rust driver built for this test run.
///
/// Nix supplies its separately packaged binary so the package check exercises
/// the installed seam. A workspace Cargo run gets the ordinary binary that
/// Cargo builds for the driver's integration tests; deriving the profile
/// directory from this test executable also respects CARGO_TARGET_DIR.
fn rust_spec_build_driver() -> PathBuf {
    let driver = std::env::var_os("TALLY_TEST_SPEC_BUILD_DRIVER")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .expect("the flow-live test executable should have a path")
                .parent()
                .and_then(Path::parent)
                .expect("the flow-live test should run from a Cargo profile directory")
                .join(format!("spec-build-driver{}", std::env::consts::EXE_SUFFIX))
        });
    assert!(
        driver.is_file(),
        "Rust spec-build driver is missing at {}; run the workspace tests so Cargo builds its integration target",
        driver.display()
    );
    let expected_name = format!("spec-build-driver{}", std::env::consts::EXE_SUFFIX);
    assert_eq!(
        driver.file_name().and_then(|name| name.to_str()),
        Some(expected_name.as_str()),
        "the Rust seam must resolve to the canonical driver executable"
    );
    driver
}

fn copy_fixture_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let target = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_fixture_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn fixture_git(directory: &Path, arguments: &[&str]) -> String {
    let output = StdCommand::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git -C {} {:?}\nstdout:\n{}\nstderr:\n{}",
        directory.display(),
        arguments,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn rpc(socket: &Path) -> RpcClient {
    RpcClient::connect(socket).await.unwrap()
}

async fn pause(client: &RpcClient, pool: &str) {
    client
        .call("queue.pause", Some(json!({"pool": pool, "all": false})))
        .await
        .unwrap();
}

async fn resume_all(client: &RpcClient) {
    client
        .call("queue.resume", Some(json!({"all": true})))
        .await
        .unwrap();
}

async fn flow_items(client: &RpcClient, flow_run_id: &str) -> Vec<Value> {
    let mut cursor: Option<String> = None;
    let mut items = Vec::new();
    loop {
        let mut params = json!({"flowRun": flow_run_id, "limit": 1000});
        if let Some(cursor) = cursor.as_ref() {
            params["cursor"] = Value::String(cursor.clone());
        }
        let page = client.call("query.jobs", Some(params)).await.unwrap();
        items.extend(page["items"].as_array().unwrap().iter().cloned());
        cursor = page["nextCursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            return items;
        }
    }
}

async fn wait_for_flow_items(client: &RpcClient, flow_run_id: &str, expected: usize) -> Vec<Value> {
    let (deadline, budget) = poll_deadline();
    loop {
        let items = flow_items(client, flow_run_id).await;
        if items.len() == expected {
            return items;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "flow run {flow_run_id} did not reach {expected} durable rows {budget}; \
             observed {}",
            items.len()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_path(path: &Path) {
    let (deadline, budget) = poll_deadline();
    loop {
        if path.exists() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "path did not appear {budget}: {}",
            path.display()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_flow_state(
    client: &RpcClient,
    flow_run_id: &str,
    expected_items: usize,
    expected_state: &str,
) -> Vec<Value> {
    let (deadline, budget) = poll_deadline();
    loop {
        let items = flow_items(client, flow_run_id).await;
        if items.len() == expected_items
            && items.iter().all(|item| item["liveState"] == expected_state)
        {
            return items;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "flow run {flow_run_id} did not reach {expected_state} state {budget}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn await_items(client: &RpcClient, items: &[Value]) {
    for item in items {
        let terminal = tokio::time::timeout(
            scaled(Duration::from_secs(20)),
            client.call(
                "queue.await_job",
                Some(json!({"task_uuid": item["anchor"]})),
            ),
        )
        .await
        .expect("node wait timed out")
        .unwrap();
        // The pristine-base preflight witness runs a gate's real
        // merge-criterion argv before any agent has built anything, so it is
        // red by construction and decides nothing. That it settled is the
        // whole assertion; its verdict is deliberately not a pass.
        if item["orchestration"]["nodeLabel"]
            .as_str()
            .is_some_and(|label| label.starts_with("preflight-witness-"))
        {
            continue;
        }
        assert_eq!(terminal["verdict"], "pass", "{terminal}");
    }
}

async fn runner_output(child: Child) -> std::process::Output {
    tokio::time::timeout(scaled(Duration::from_secs(60)), child.wait_with_output())
        .await
        .expect("flow runner timed out")
        .unwrap()
}

fn flow_report(output: &std::process::Output) -> Value {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["type"] == "flow-report")
        .unwrap_or_else(|| {
            panic!(
                "runner omitted flow-report\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
}

fn runner_events(output: &std::process::Output, kind: &str) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|event| event["type"] == kind)
        .collect()
}

fn flow_failure(output: &std::process::Output) -> Value {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|event| event["type"] == "flow-failed")
        .unwrap_or_else(|| {
            panic!(
                "runner omitted flow-failed\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
}

fn capture(paths: &DaemonPaths, task_uuid: &str) -> String {
    capture_stem(paths, task_uuid)
}

fn task_capture(paths: &DaemonPaths, task_uuid: &str, task_id: &str) -> String {
    capture_stem(paths, &format!("{task_uuid}.{task_id}"))
}

fn capture_stem(paths: &DaemonPaths, stem: &str) -> String {
    let stdout = fs::read_to_string(paths.state_dir.join("capture").join(format!("{stem}.out")))
        .unwrap_or_default();
    let stderr = fs::read_to_string(paths.state_dir.join("capture").join(format!("{stem}.err")))
        .unwrap_or_default();
    format!("stdout:\n{stdout}\nstderr:\n{stderr}")
}

fn assert_six_unique_rows(paths: &DaemonPaths, flow_run_id: &str) {
    let events = read_acknowledged_events(&paths.events_dir()).unwrap();
    let rows = events
        .iter()
        .filter(|event| {
            event
                .row
                .orchestration
                .as_ref()
                .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 6, "one durable enqueue event per ordinal");
    let mut ordinals = rows
        .iter()
        .map(|event| {
            event.row.orchestration.as_ref().unwrap().as_value()["nodeOrdinal"]
                .as_u64()
                .unwrap()
        })
        .collect::<Vec<_>>();
    ordinals.sort_unstable();
    assert_eq!(ordinals, [0, 1, 2, 3, 4, 5]);

    let (report, records) = read_verified_records(&paths.witness_path()).unwrap();
    assert!(report.ok);
    let witnessed = records
        .iter()
        .filter(|record| {
            record
                .orchestration
                .as_ref()
                .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(witnessed.len(), 6, "one terminal witness per ordinal");
}

#[tokio::test(flavor = "current_thread")]
async fn fs5_live_acceptance_matrix() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let paths = paths(temp.path());
            let config = config();
            let config_path = temp.path().join("config.json");
            let six_node_script = temp.path().join("six-node.js");
            fs::write(&six_node_script, six_node_source()).unwrap();
            let divergent_script = temp.path().join("divergent.js");
            fs::write(&divergent_script, divergent_source()).unwrap();
            config.validate().unwrap();
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

            let mut daemon = start_daemon(&paths, config.clone()).await;
            let mut client = rpc(&paths.socket).await;

            // A flow runner is itself an ordinary daemon job. Its task UUID becomes
            // flowRunId, and every child carries real ancestry and orchestration.
            let parent = client
                .call(
                    "queue.enqueue",
                    Some(json!({
                        "argv": [
                            env!("CARGO_BIN_EXE_tally"),
                            "--config", config_path,
                            "--socket", paths.socket,
                            "flow", "run", six_node_script,
                            "--args", "{}",
                            "--max-nodes", "6"
                        ],
                        "pool": ["flow"],
                        "adapter": "shell",
                        "source": "manual",
                        "dedupKey": "fs5-runner-as-job",
                        "evidence": ["exit:0"],
                        "noEnqueue": false,
                        "wait": false
                    })),
                )
                .await
                .unwrap();
            let parent_uuid = parent["task_uuid"].as_str().unwrap().to_owned();
            let parent_terminal = tokio::time::timeout(
                scaled(Duration::from_secs(30)),
                client.call("queue.await_job", Some(json!({"task_uuid": parent_uuid}))),
            )
            .await
            .expect("runner-as-job timed out")
            .unwrap();
            assert_eq!(
                parent_terminal["verdict"],
                "pass",
                "{}",
                capture(&paths, &parent_uuid)
            );
            let parent_children = wait_for_flow_items(&client, &parent_uuid, 6).await;
            for child in &parent_children {
                assert_eq!(child["source"], "orchestrator");
                assert_eq!(child["parentTaskUuid"], parent_uuid);
                assert_eq!(child["orchestration"]["flowRunId"], child["parentTaskUuid"]);
                assert!(child["orchestration"]["argsHash"]
                    .as_str()
                    .is_some_and(|hash| hash.starts_with("sha256:")));
                assert!(child["orchestration"]["catalogHash"].is_null());
                assert_eq!(child["noEnqueue"], true);
                assert!(child.get("relatedTrigger").is_none());
            }
            assert_six_unique_rows(&paths, &parent_uuid);

            // Two concurrent runners race every ordinal while work is paused. The
            // kernel creates one row and returns attach to the other runner.
            pause(&client, "alpha").await;
            pause(&client, "beta").await;
            let first = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                CONCURRENT_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            let second = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                CONCURRENT_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            wait_for_flow_items(&client, CONCURRENT_RUN, 6).await;
            resume_all(&client).await;
            let (first, second) = tokio::join!(runner_output(first), runner_output(second));
            assert!(
                first.status.success(),
                "{}",
                String::from_utf8_lossy(&first.stderr)
            );
            assert!(
                second.status.success(),
                "{}",
                String::from_utf8_lossy(&second.stderr)
            );
            assert!(String::from_utf8_lossy(&first.stdout).contains("\"type\":\"flow-report\""));
            assert!(String::from_utf8_lossy(&second.stdout).contains("\"type\":\"flow-report\""));
            assert_eq!(flow_items(&client, CONCURRENT_RUN).await.len(), 6);
            assert_six_unique_rows(&paths, CONCURRENT_RUN);

            // SIGKILL loses only the stateless runner. The six durable child rows
            // finish, and retry collapses the whole prefix without a second row.
            pause(&client, "alpha").await;
            pause(&client, "beta").await;
            let mut killed = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                KILLED_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            let killed_items = wait_for_flow_items(&client, KILLED_RUN, 6).await;
            killed.kill().await.unwrap();
            resume_all(&client).await;
            await_items(&client, &killed_items).await;
            let replay = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                KILLED_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            let replay = runner_output(replay).await;
            assert!(
                replay.status.success(),
                "{}",
                String::from_utf8_lossy(&replay.stderr)
            );
            assert_six_unique_rows(&paths, KILLED_RUN);

            // A daemon epoch change tears down all six outstanding awaits. The
            // runner reconnects one replacement client, re-awaits, and completes.
            pause(&client, "alpha").await;
            pause(&client, "beta").await;
            let restarted = runner(
                &config_path,
                &paths.socket,
                &six_node_script,
                RESTARTED_RUN,
                "{}",
                6,
            )
            .spawn()
            .unwrap();
            wait_for_flow_items(&client, RESTARTED_RUN, 6).await;
            drop(client);
            daemon.stop().await;
            daemon = start_daemon(&paths, config.clone()).await;
            let restarted = runner_output(restarted).await;
            assert!(
                restarted.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&restarted.stdout),
                String::from_utf8_lossy(&restarted.stderr)
            );
            assert!(
                String::from_utf8_lossy(&restarted.stdout)
                    .lines()
                    .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                    .any(|event| event["type"] == "flow-rpc-reconnect"),
                "restart emitted no reconnect lifecycle event:\n{}",
                String::from_utf8_lossy(&restarted.stdout)
            );
            client = rpc(&paths.socket).await;
            assert_eq!(flow_items(&client, RESTARTED_RUN).await.len(), 6);
            assert_six_unique_rows(&paths, RESTARTED_RUN);

            // Changed arguments are rejected by the run-level identity scan before
            // a re-derived node can reach admission.
            pause(&client, "alpha").await;
            let mut original = runner(
                &config_path,
                &paths.socket,
                &divergent_script,
                DIVERGENT_RUN,
                r#"{"variant":"recorded"}"#,
                1,
            )
            .spawn()
            .unwrap();
            let original_items = wait_for_flow_items(&client, DIVERGENT_RUN, 1).await;
            original.kill().await.unwrap();
            let divergent = runner(
                &config_path,
                &paths.socket,
                &divergent_script,
                DIVERGENT_RUN,
                r#"{"variant":"expected"}"#,
                1,
            )
            .spawn()
            .unwrap();
            let divergent = runner_output(divergent).await;
            assert_eq!(divergent.status.code(), Some(20));
            let failed = String::from_utf8(divergent.stdout).unwrap();
            let event = failed
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .find(|event| event["type"] == "flow-failed")
                .unwrap();
            assert_eq!(event["error"]["code"], "args-changed-mid-run");
            assert!(event["error"]["details"]["currentHash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));
            assert!(event["error"]["details"]["recordedHash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));
            assert_eq!(flow_items(&client, DIVERGENT_RUN).await.len(), 1);
            resume_all(&client).await;
            await_items(&client, &original_items).await;

            // Script identity is inspected from durable rows before re-execution.
            fs::write(
                &divergent_script,
                format!("{}\n// edited generation\n", divergent_source()),
            )
            .unwrap();
            let edited = runner(
                &config_path,
                &paths.socket,
                &divergent_script,
                DIVERGENT_RUN,
                r#"{"variant":"recorded"}"#,
                1,
            )
            .spawn()
            .unwrap();
            let edited = runner_output(edited).await;
            assert_eq!(edited.status.code(), Some(20));
            assert!(String::from_utf8_lossy(&edited.stdout).contains("script-changed-mid-run"));
            assert_eq!(flow_items(&client, DIVERGENT_RUN).await.len(), 1);
            // The refusal carries its own recovery instruction, so a supervisor
            // never has to read the message to know retrying is pointless.
            let edited_error = flow_failure(&edited)["error"].clone();
            assert_eq!(edited_error["details"]["divergentInput"], "script");
            assert_eq!(edited_error["details"]["transient"], false);
            assert_eq!(edited_error["details"]["resolution"], "supersede");

            // The generation boundary: a durable rollover from the terminal old
            // run to a fresh successor, recorded once and safe to repeat.
            let supersede = || {
                Command::new(env!("CARGO_BIN_EXE_tally"))
                    .arg("--config")
                    .arg(&config_path)
                    .arg("--socket")
                    .arg(&paths.socket)
                    .args(["flow", "supersede"])
                    .args(["--flow-run-id", DIVERGENT_RUN])
                    .args(["--new-flow-run-id", DIVERGENT_SUCCESSOR_RUN])
                    .args(["--reason", "generation-change"])
                    .output()
            };
            let recorded = supersede().await.unwrap();
            assert!(
                recorded.status.success(),
                "{}",
                String::from_utf8_lossy(&recorded.stderr)
            );
            let recorded: Value = serde_json::from_slice(&recorded.stdout).unwrap();
            assert_eq!(recorded["disposition"], "recorded");
            assert_eq!(
                recorded["record"]["successorFlowRunId"],
                DIVERGENT_SUCCESSOR_RUN
            );
            assert_eq!(recorded["record"]["reason"], "generation-change");
            let repeated = supersede().await.unwrap();
            assert!(repeated.status.success());
            let repeated: Value = serde_json::from_slice(&repeated.stdout).unwrap();
            assert_eq!(repeated["disposition"], "reused");
            assert_eq!(repeated["record"], recorded["record"]);

            // Replaying the retired ID now names its successor instead of
            // re-reporting which hash moved.
            let retired = runner(
                &config_path,
                &paths.socket,
                &divergent_script,
                DIVERGENT_RUN,
                r#"{"variant":"recorded"}"#,
                1,
            )
            .spawn()
            .unwrap();
            let retired = runner_output(retired).await;
            assert_eq!(retired.status.code(), Some(20));
            let retired_error = flow_failure(&retired)["error"].clone();
            assert_eq!(retired_error["code"], "flow-run-superseded");
            assert_eq!(
                retired_error["details"]["successorFlowRunId"],
                DIVERGENT_SUCCESSOR_RUN
            );
            assert_eq!(retired_error["details"]["resolution"], "run-successor");
            // The old run is untouched: one durable row, exactly as before.
            assert_eq!(flow_items(&client, DIVERGENT_RUN).await.len(), 1);

            // The successor runs the new generation's script to completion.
            let successor = runner(
                &config_path,
                &paths.socket,
                &divergent_script,
                DIVERGENT_SUCCESSOR_RUN,
                r#"{"variant":"successor"}"#,
                1,
            )
            .spawn()
            .unwrap();
            let successor = runner_output(successor).await;
            assert!(
                successor.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&successor.stdout),
                String::from_utf8_lossy(&successor.stderr)
            );
            assert_eq!(
                flow_report(&successor)["report"]["flowRunId"],
                DIVERGENT_SUCCESSOR_RUN
            );
            assert_eq!(
                flow_items(&client, DIVERGENT_SUCCESSOR_RUN).await.len(),
                1,
                "the successor starts fresh rather than adopting old nodes"
            );

            // Both query surfaces answer the generation question unambiguously.
            let lineage = Command::new(env!("CARGO_BIN_EXE_tally"))
                .arg("--config")
                .arg(&config_path)
                .arg("--socket")
                .arg(&paths.socket)
                .args(["query", "lineage", DIVERGENT_RUN])
                .output()
                .await
                .unwrap();
            assert!(
                lineage.status.success(),
                "{}",
                String::from_utf8_lossy(&lineage.stderr)
            );
            let lineage: Value = serde_json::from_slice(&lineage.stdout).unwrap();
            assert_eq!(lineage["superseded"], true);
            assert_eq!(lineage["currentFlowRunId"], DIVERGENT_SUCCESSOR_RUN);
            assert_eq!(
                lineage["chain"],
                json!([DIVERGENT_RUN, DIVERGENT_SUCCESSOR_RUN])
            );
            let retired_view = client
                .call("query.run", Some(json!({"id": DIVERGENT_RUN})))
                .await
                .unwrap();
            assert_eq!(retired_view["state"], "superseded");
            assert_eq!(
                retired_view["supersededBy"]["successorFlowRunId"],
                DIVERGENT_SUCCESSOR_RUN
            );
            let successor_view = client
                .call("query.run", Some(json!({"id": DIVERGENT_SUCCESSOR_RUN})))
                .await
                .unwrap();
            assert_ne!(successor_view["state"], "superseded");
            assert_eq!(successor_view["supersedes"]["flowRunId"], DIVERGENT_RUN);

            let events = read_acknowledged_events(&paths.events_dir()).unwrap();
            let parent_event = events
                .iter()
                .find(|event| event.row.uuid.to_string() == parent_uuid)
                .unwrap();
            assert_eq!(parent_event.row.source, EnqueueSource::Manual);
            let child_events = events
                .iter()
                .filter(|event| {
                    event
                        .row
                        .parent_uuid
                        .map(|uuid| uuid.to_string())
                        .as_deref()
                        == Some(&parent_uuid)
                })
                .collect::<Vec<_>>();
            assert_eq!(child_events.len(), 6);
            assert!(child_events
                .iter()
                .all(|event| event.row.source == EnqueueSource::Orchestrator));

            daemon.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn credentialed_pool_replays_the_same_flow_as_reused() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let daemon_paths = paths(&temp.path().join("daemon"));
            let credential = temp.path().join("alpha-token");
            fs::write(&credential, "test-only-token").unwrap();

            let mut config = config();
            config.attestations.exec.enable = false;
            config
                .pools
                .get_mut("alpha")
                .unwrap()
                .credentials
                .insert("api-token".to_owned(), credential.clone());
            config.validate().unwrap();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let script = temp.path().join("credential-replay.js");
            fs::write(&script, task_ref_one_node_source()).unwrap();
            let systemd_run = install_fake_systemd_run(temp.path(), &daemon_paths.state_dir);
            let daemon = start_daemon_with_systemd_run(&daemon_paths, config, systemd_run).await;

            let first = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                CREDENTIAL_REPLAY_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let first = runner_output(first).await;
            assert!(
                first.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&first.stdout),
                String::from_utf8_lossy(&first.stderr)
            );
            assert_eq!(
                flow_report(&first)["report"]["finalValue"]["disposition"],
                "created"
            );

            let second = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                CREDENTIAL_REPLAY_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let second = runner_output(second).await;
            assert!(
                second.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&second.stdout),
                String::from_utf8_lossy(&second.stderr)
            );
            assert_eq!(
                flow_report(&second)["report"]["finalValue"]["disposition"],
                "reused"
            );

            // The runner's own stream reports each node's disposition, so a
            // replayed prefix is visible without inspecting the daemon.
            let submitted = runner_events(&second, "node-submitted");
            assert_eq!(submitted.len(), 1);
            assert_eq!(submitted[0]["disposition"], "reused");
            assert_eq!(submitted[0]["ordinal"], 0);
            assert_eq!(submitted[0]["taskRef"], "crm/t07");
            assert_eq!(
                submitted[0]["dedupKey"],
                format!("flow:{CREDENTIAL_REPLAY_RUN}:0")
            );
            let terminal = runner_events(&second, "node-terminal");
            assert_eq!(terminal.len(), 1);
            assert_eq!(terminal[0]["disposition"], "reused");
            assert_eq!(terminal[0]["verdict"], "pass");
            assert_eq!(terminal[0]["taskRef"], "crm/t07");
            assert_eq!(terminal[0]["taskUuid"], submitted[0]["taskUuid"]);

            let events = read_acknowledged_events(&daemon_paths.events_dir()).unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].row.credentials["api-token"], credential);

            // Replay wrote no second row, and the one row carries the node's
            // dedup key and the disposition that created it.
            let client = rpc(&daemon_paths.socket).await;
            let items = flow_items(&client, CREDENTIAL_REPLAY_RUN).await;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0]["taskRef"], "crm/t07");
            assert_eq!(
                items[0]["unit"],
                format!(
                    "tally-job-crm-t07-{}.service",
                    items[0]["taskUuid"].as_str().unwrap()
                )
            );
            assert_eq!(
                items[0]["dedupKey"],
                format!("flow:{CREDENTIAL_REPLAY_RUN}:0")
            );
            assert_eq!(items[0]["disposition"], "created");

            let log = client
                .call(
                    "query.log",
                    Some(json!({"flowRun": CREDENTIAL_REPLAY_RUN, "limit": 1000})),
                )
                .await
                .unwrap();
            let logged = log["items"].as_array().unwrap();
            assert!(!logged.is_empty());
            assert!(logged
                .iter()
                .all(|event| event["taskUuid"] == items[0]["taskUuid"]));

            let proofs = client
                .call(
                    "query.proof",
                    Some(json!({"flowRun": CREDENTIAL_REPLAY_RUN})),
                )
                .await
                .unwrap();
            let proofs = proofs["items"].as_array().unwrap();
            assert_eq!(proofs.len(), 1);
            assert_eq!(proofs[0]["taskUuid"], items[0]["taskUuid"]);

            daemon.stop().await;
        })
        .await;
}

/// The one wire rename in the exit-20 family, proved by a real process.
///
/// `replay-divergence` renamed `expectedHash`/`expectedLabel` to
/// `currentHash`/`currentLabel`, and every other live exit-20 assertion in this
/// suite is an identity pin — so the renamed members were only ever exercised
/// against in-process stubs, which agree with whatever name the code picks. This
/// drives a genuine daemon, a genuine runner process, and a genuine ledger
/// conflict, and reads the names off the runner's own stdout.
///
/// Reaching a payload divergence needs an input that is inside the canonical
/// payload and outside the three identity hashes, or the startup pins refuse the
/// replay first. A pool credential is exactly that: it is hashed into the
/// payload and it comes from the client config rather than from the script, the
/// args, or the catalog. Granting the pool a second credential between the two
/// runs — an ordinary operator edit — is therefore the smallest honest way to
/// make one admitted ordinal re-derive different work.
#[tokio::test(flavor = "current_thread")]
async fn a_live_replay_divergence_names_the_current_hash_and_label_on_the_wire() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let daemon_paths = paths(&temp.path().join("daemon"));
            let recorded_credential = temp.path().join("alpha-token");
            fs::write(&recorded_credential, "test-only-token").unwrap();
            let granted_credential = temp.path().join("alpha-second-token");
            fs::write(&granted_credential, "test-only-token").unwrap();

            let mut config = config();
            config.attestations.exec.enable = false;
            config
                .pools
                .get_mut("alpha")
                .unwrap()
                .credentials
                .insert("api-token".to_owned(), recorded_credential);
            config.validate().unwrap();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

            // Same script, same args, same (absent) catalog — so the run-level
            // identity scan admits the replay and the divergence is about the
            // ordinal's payload rather than about the run's identity. The pool's
            // original credential is left exactly as the daemon knows it, so the
            // admission is refused for the divergence and not for a credential
            // the daemon and the client disagree about.
            let mut replay_config = config.clone();
            replay_config
                .pools
                .get_mut("alpha")
                .unwrap()
                .credentials
                .insert("rotation-token".to_owned(), granted_credential);
            replay_config.validate().unwrap();
            let replay_config_path = temp.path().join("replay-config.json");
            fs::write(
                &replay_config_path,
                serde_json::to_vec(&replay_config).unwrap(),
            )
            .unwrap();

            let script = temp.path().join("replay-divergence.js");
            fs::write(&script, task_ref_one_node_source()).unwrap();
            let systemd_run = install_fake_systemd_run(temp.path(), &daemon_paths.state_dir);
            let daemon = start_daemon_with_systemd_run(&daemon_paths, config, systemd_run).await;

            let recorded = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                REPLAY_DIVERGENCE_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let recorded = runner_output(recorded).await;
            assert!(
                recorded.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&recorded.stdout),
                String::from_utf8_lossy(&recorded.stderr)
            );
            assert_eq!(
                flow_report(&recorded)["report"]["finalValue"]["disposition"],
                "created"
            );
            let recorded_submission = runner_events(&recorded, "node-submitted")[0].clone();
            let recorded_task_uuid = recorded_submission["taskUuid"].as_str().unwrap().to_owned();
            // The payload the ledger holds, taken from the *first* runner's own
            // stdout. This is the independent oracle that tells the two sides of
            // the divergence apart: the second runner never sees this value
            // except through the refusal it is being checked against.
            let ledger_payload_hash = recorded_submission["payloadHash"]
                .as_str()
                .unwrap()
                .to_owned();

            let diverged = runner(
                &replay_config_path,
                &daemon_paths.socket,
                &script,
                REPLAY_DIVERGENCE_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let diverged = runner_output(diverged).await;
            assert_eq!(
                diverged.status.code(),
                Some(20),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&diverged.stdout),
                String::from_utf8_lossy(&diverged.stderr)
            );
            let error = flow_failure(&diverged)["error"].clone();
            assert_eq!(error["code"], "replay-divergence");
            assert_eq!(error["name"], "FlowReplayError");
            assert_eq!(error["ordinal"], 0);

            let details = error["details"].as_object().unwrap();
            // The whole contract, in emission order, off a real process's stdout.
            assert_eq!(
                details.keys().map(String::as_str).collect::<Vec<_>>(),
                SUPERSESSION_DETAIL_FIELDS
            );
            // The rename, on the wire: the two sides of the disagreement are
            // named `currentHash`/`currentLabel`, and the pre-rename names are
            // absent. These four are spelled out rather than read through the
            // constant on purpose — comparing the wire with the constant the
            // wire is built from would agree with whatever name the code picked.
            // Which member holds which side is bound for the hashes below, and
            // deliberately not for the labels; see there.
            let current_hash = details
                .get("currentHash")
                .and_then(Value::as_str)
                .expect("a live replay divergence names currentHash on the wire");
            let recorded_hash = details
                .get("recordedHash")
                .and_then(Value::as_str)
                .expect("a live replay divergence names recordedHash on the wire");
            assert!(current_hash.starts_with("sha256:"));
            assert!(recorded_hash.starts_with("sha256:"));
            assert_ne!(current_hash, recorded_hash);
            // Which member carries which side, bound against the first runner's
            // own report rather than against the refusal itself. Asserting only
            // that the two hashes differ would leave a swap at the raising site
            // invisible here — which is what the comment above would then be
            // claiming without evidence.
            assert_eq!(
                recorded_hash, ledger_payload_hash,
                "recordedHash must be the payload the ledger already held"
            );
            assert_ne!(
                current_hash, ledger_payload_hash,
                "currentHash must be the payload this runner re-derived"
            );
            // The labels are pinned by name only. This fixture is structurally
            // blind to a *label* swap: the recorded and current labels come from
            // the same unchanged script, so they are the same string here. Making
            // them differ would mean changing the script between the two runs,
            // which the startup identity pin refuses before an ordinal is ever
            // admitted, so no scenario in this suite can see it.
            //
            // What compensates for that blindness has to guard *this* refusal's
            // raising site, and this refusal carries a `kernelError` (asserted
            // below), so it is raised by the dedup-conflict path in
            // `crates/tally/src/flow_live.rs::submission_error`. The tests that
            // bind the two label members there, against two different strings,
            // are in that same file:
            // `flow_live::tests::matching_kernel_conflict_becomes_replay_divergence_with_both_labels`
            // and `flow_live::tests::every_exit_twenty_refusal_carries_the_same_details_contract`.
            // Swap the two members at that site and both go red.
            //
            // `payload_divergence_stops_admission_at_the_mismatched_ordinal`
            // (crates/tally-flow/src/engine/tests.rs) binds the same two members
            // additionally at the *runner's* own comparison site, which this
            // fixture never reaches — extra coverage of a sibling site, not the
            // guard for this one.
            assert_eq!(
                details.get("currentLabel").and_then(Value::as_str),
                Some("task-ref-child")
            );
            assert_eq!(
                details.get("recordedLabel").and_then(Value::as_str),
                Some("task-ref-child")
            );
            assert!(details.get("expectedHash").is_none());
            assert!(details.get("expectedLabel").is_none());
            // The rest of the family contract, from the same real refusal.
            assert_eq!(details["flowRunId"], REPLAY_DIVERGENCE_RUN);
            assert_eq!(details["divergentInput"], "payload");
            assert_eq!(details["taskUuid"], recorded_task_uuid);
            assert!(details["kernelError"]
                .as_str()
                .unwrap()
                .contains("dedup-key-conflict"));
            assert_eq!(details["transient"], false);
            assert_eq!(details["resolution"], "investigate");
            // A rollover does not clear a divergence, so there is no command to
            // advertise even though the refusal knows exactly which run it is.
            assert!(details["remedy"].is_null());

            // The refusal wrote no second row: the ledger still holds the one
            // node the recorded run created.
            let client = rpc(&daemon_paths.socket).await;
            let items = flow_items(&client, REPLAY_DIVERGENCE_RUN).await;
            assert_eq!(items.len(), 1);
            assert_eq!(items[0]["taskUuid"], recorded_task_uuid);

            daemon.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn drv_second_run_substitutes_without_a_second_build() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let (_marker, builds, _path_guard) = install_fake_nix(temp.path());
            let config = config();
            config.validate().unwrap();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let script = temp.path().join("drv.js");
            fs::write(&script, drv_source()).unwrap();

            let daemon_paths = paths(&temp.path().join("daemon"));
            let daemon = start_daemon(&daemon_paths, config).await;
            let first = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                DRV_BUILD_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let first = runner_output(first).await;
            assert!(
                first.status.success(),
                "{}",
                String::from_utf8_lossy(&first.stderr)
            );
            let first_report = flow_report(&first);
            assert_eq!(
                first_report["report"]["finalValue"]["disposition"],
                "created"
            );
            assert_eq!(first_report["report"]["finalValue"]["verdict"], "pass");
            assert_eq!(
                first_report["report"]["finalValue"]["taskUuid"],
                "39cd245e-fb7a-5bf0-8b59-46475d6ff96e"
            );

            assert_eq!(fs::read_to_string(&builds).unwrap().lines().count(), 1);
            let first_events = read_acknowledged_events(&daemon_paths.events_dir()).unwrap();
            assert_eq!(first_events.len(), 1);
            assert_eq!(first_events[0].row.pools, ["build"]);
            assert_eq!(
                first_events[0].row.dedup_key.as_deref(),
                Some(format!("drv:{DRV_PATH}").as_str())
            );
            let (report, records) = read_verified_records(&daemon_paths.witness_path()).unwrap();
            assert!(report.ok);
            assert_eq!(records.len(), 1);
            assert_eq!(records[0].verdict, tally_core::witness::Verdict::Pass);
            assert_eq!(records[0].store_paths, Some(vec![DRV_OUTPUT.to_owned()]));

            let second = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                DRV_SUBSTITUTE_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let second = runner_output(second).await;
            assert!(
                second.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&second.stdout),
                String::from_utf8_lossy(&second.stderr)
            );
            let second_report = flow_report(&second);
            assert_eq!(
                second_report["report"]["finalValue"]["disposition"],
                "substituted"
            );
            assert_eq!(
                second_report["report"]["finalValue"]["verdict"],
                "substituted"
            );
            assert_eq!(
                second_report["report"]["finalValue"]["taskUuid"],
                "63c56d72-e3bf-5bcf-93c6-1577d6a20f8d"
            );
            daemon.stop().await;

            assert_eq!(
                fs::read_to_string(&builds).unwrap().lines().count(),
                1,
                "the store-native second run must not execute nix build"
            );
            let events = read_acknowledged_events(&daemon_paths.events_dir()).unwrap();
            assert_eq!(
                events.len(),
                1,
                "the substituted fast path must not admit a second row"
            );
            assert_eq!(
                events[0].row.orchestration.as_ref().unwrap().flow_run_id(),
                DRV_BUILD_RUN
            );
            let (report, records) = read_verified_records(&daemon_paths.witness_path()).unwrap();
            assert!(report.ok);
            assert_eq!(records.len(), 2);
            assert_ne!(records[0].task_uuid, records[1].task_uuid);
            assert_eq!(
                records[1].verdict,
                tally_core::witness::Verdict::Substituted
            );
            assert_eq!(records[1].pools, ["build"]);
            assert_eq!(
                records[1].dedup_key.as_deref(),
                Some(format!("drv:{DRV_PATH}").as_str())
            );
            assert_eq!(records[1].drv.as_ref().unwrap().drv_path, DRV_PATH);
            assert_eq!(records[1].store_paths, Some(vec![DRV_OUTPUT.to_owned()]));
            assert_eq!(
                records[1].orchestration.as_ref().unwrap().flow_run_id(),
                DRV_SUBSTITUTE_RUN
            );
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn structured_result_is_observed_after_terminal_ack_and_replayed_after_restart() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let mut config = config();
            config.adapters.insert(
                "structured".to_owned(),
                AdapterConfig {
                    scrape: BTreeMap::from([(
                        "finalMessage".to_owned(),
                        ScrapeCapture {
                            stream: ScrapeStream::Stdout,
                            mode: ScrapeMode::JsonPath,
                            pattern: "$..final_message".to_owned(),
                            counter_scope: None,
                            fields: Default::default(),
                        },
                    )]),
                    ..AdapterConfig::default()
                },
            );
            config.validate().unwrap();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let script = temp.path().join("structured-result.js");
            fs::write(&script, structured_result_source()).unwrap();
            let daemon_paths = paths(&temp.path().join("daemon"));

            let daemon = start_daemon(&daemon_paths, config.clone()).await;
            let first = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                STRUCTURED_REPLAY_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let first = runner_output(first).await;
            assert!(
                first.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&first.stdout),
                String::from_utf8_lossy(&first.stderr)
            );
            assert_eq!(
                flow_report(&first)["report"]["finalValue"],
                json!({"answer": 42})
            );
            daemon.stop().await;

            let capture_dir = daemon_paths.state_dir.join("capture");
            if capture_dir.exists() {
                fs::remove_dir_all(capture_dir).unwrap();
            }

            let restarted = start_daemon(&daemon_paths, config).await;
            let replay = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                STRUCTURED_REPLAY_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let replay = runner_output(replay).await;
            assert!(
                replay.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&replay.stdout),
                String::from_utf8_lossy(&replay.stderr)
            );
            assert_eq!(
                flow_report(&replay)["report"]["finalValue"],
                json!({"answer": 42})
            );
            assert_eq!(
                read_acknowledged_events(&daemon_paths.events_dir())
                    .unwrap()
                    .len(),
                1,
                "result replay must not materialize a second row"
            );
            restarted.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn regex_result_is_observed_after_terminal_ack_without_restart() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let mut config = config();
            config.adapters.insert(
                "ocr-driver".to_owned(),
                AdapterConfig {
                    scrape: BTreeMap::from([(
                        "finalMessage".to_owned(),
                        ScrapeCapture {
                            stream: ScrapeStream::Stdout,
                            mode: ScrapeMode::Regex,
                            pattern: "^TALLY_FINAL_MESSAGE=(.*)$".to_owned(),
                            counter_scope: None,
                            fields: Default::default(),
                        },
                    )]),
                    ..AdapterConfig::default()
                },
            );
            config.validate().unwrap();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let script = temp.path().join("regex-result.js");
            fs::write(&script, regex_result_source()).unwrap();
            let daemon_paths = paths(&temp.path().join("daemon"));

            let daemon = start_daemon(&daemon_paths, config).await;
            let output = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                REGEX_RESULT_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let output = runner_output(output).await;
            assert!(
                output.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                flow_report(&output)["report"]["finalValue"],
                json!({"ok": true, "n": 3})
            );
            let (report, attestations) =
                read_verified_attestations(&daemon_paths.attestations_path()).unwrap();
            assert!(report.ok);
            assert_eq!(attestations.len(), 1);
            assert_eq!(attestations[0].payload["kind"], "adapter-scrape");
            assert_eq!(
                attestations[0].payload["captures"]["finalMessage"],
                r#"{"ok":true,"n":3}"#
            );
            assert!(attestations[0]
                .payload
                .get("reconciledAfterRestart")
                .is_none());
            daemon.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn untyped_final_message_is_observed_after_terminal_ack() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let mut config = config();
            config.adapters.insert(
                "structured".to_owned(),
                AdapterConfig {
                    scrape: BTreeMap::from([(
                        "finalMessage".to_owned(),
                        ScrapeCapture {
                            stream: ScrapeStream::Stdout,
                            mode: ScrapeMode::JsonPath,
                            pattern: "$..final_message".to_owned(),
                            counter_scope: None,
                            fields: Default::default(),
                        },
                    )]),
                    ..AdapterConfig::default()
                },
            );
            config.validate().unwrap();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let script = temp.path().join("untyped-result.js");
            fs::write(&script, untyped_result_source()).unwrap();
            let daemon_paths = paths(&temp.path().join("daemon"));

            let daemon = start_daemon(&daemon_paths, config).await;
            let output = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                UNTYPED_RESULT_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let output = runner_output(output).await;
            assert!(
                output.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                flow_report(&output)["report"]["finalValue"],
                json!({"answer": 42})
            );
            daemon.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn retry_cancellation_cap_and_partial_failure_are_live_end_to_end() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let config = config();
            config.validate().unwrap();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

            let automatic_script = temp.path().join("automatic-requeue.js");
            fs::write(
                &automatic_script,
                automatic_requeue_source(&temp.path().join("attempt-one-seen")),
            )
            .unwrap();
            let cancellation_script = temp.path().join("cancellation.js");
            fs::write(&cancellation_script, cancellation_source()).unwrap();
            let cap_script = temp.path().join("cancelled-cap-replay.js");
            fs::write(&cap_script, cancelled_cap_replay_source()).unwrap();
            let partial_script = temp.path().join("partial-parallel-failure.js");
            fs::write(&partial_script, partial_parallel_failure_source()).unwrap();
            let reordered_script = temp.path().join("reordered-parallel.js");
            fs::write(&reordered_script, reordered_parallel_source()).unwrap();
            let catalog_script = temp.path().join("catalog-pin.js");
            fs::write(&catalog_script, catalog_pin_source()).unwrap();
            let catalog_path = temp.path().join("catalog.json");
            fs::write(&catalog_path, r#"{"version":1,"members":[]}"#).unwrap();

            let daemon_paths = paths(&temp.path().join("daemon"));
            let mut retry_settings = settings();
            retry_settings.recovery_policy.retry.auto_bounded_requeue = true;
            retry_settings.recovery_policy.max_attempts = 2;
            let daemon = start_daemon_with_settings(&daemon_paths, config, retry_settings).await;
            let client = rpc(&daemon_paths.socket).await;

            let automatic = runner(
                &config_path,
                &daemon_paths.socket,
                &automatic_script,
                AUTO_REQUEUE_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let automatic = runner_output(automatic).await;
            assert!(
                automatic.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&automatic.stdout),
                String::from_utf8_lossy(&automatic.stderr)
            );
            let events = read_acknowledged_events(&daemon_paths.events_dir()).unwrap();
            let automatic_event = events
                .iter()
                .find(|event| {
                    event
                        .row
                        .orchestration
                        .as_ref()
                        .is_some_and(|orchestration| {
                            orchestration.flow_run_id() == AUTO_REQUEUE_RUN
                        })
                })
                .unwrap();
            assert_eq!(automatic_event.retries.len(), 1);
            assert_eq!(automatic_event.retries[0].attempt, 2);
            let (_, witnesses) = read_verified_records(&daemon_paths.witness_path()).unwrap();
            let automatic_witnesses = witnesses
                .iter()
                .filter(|record| {
                    record.orchestration.as_ref().is_some_and(|orchestration| {
                        orchestration.flow_run_id() == AUTO_REQUEUE_RUN
                    })
                })
                .collect::<Vec<_>>();
            assert_eq!(automatic_witnesses.len(), 2);
            assert_eq!(automatic_witnesses[0].attempt, 1);
            assert_eq!(
                automatic_witnesses[0].verdict,
                tally_core::witness::Verdict::RuntimeExceeded
            );
            assert_eq!(automatic_witnesses[1].attempt, 2);
            assert_eq!(
                automatic_witnesses[1].verdict,
                tally_core::witness::Verdict::Pass
            );

            let cancelled_runner = runner(
                &config_path,
                &daemon_paths.socket,
                &cancellation_script,
                CANCELLED_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            wait_for_flow_state(&client, CANCELLED_RUN, 1, "running").await;
            let cancel_output = Command::new(env!("CARGO_BIN_EXE_tally"))
                .arg("--config")
                .arg(&config_path)
                .arg("--socket")
                .arg(&daemon_paths.socket)
                .args(["flow", "cancel", CANCELLED_RUN])
                .output()
                .await
                .unwrap();
            assert!(
                cancel_output.status.success(),
                "{}",
                String::from_utf8_lossy(&cancel_output.stderr)
            );
            let cancel_response: Value = serde_json::from_slice(&cancel_output.stdout).unwrap();
            assert_eq!(cancel_response["affected"], 1);
            assert_eq!(cancel_response["flowRunId"], CANCELLED_RUN);
            assert_eq!(cancel_response["results"][0]["was"], "running");
            let cancelled_runner = runner_output(cancelled_runner).await;
            assert_eq!(cancelled_runner.status.code(), Some(4));
            assert_eq!(
                flow_failure(&cancelled_runner)["error"]["code"],
                "flow-cancelled"
            );
            let (_, witnesses) = read_verified_records(&daemon_paths.witness_path()).unwrap();
            assert!(witnesses.iter().any(|record| {
                record.verdict == tally_core::witness::Verdict::Cancelled
                    && record
                        .orchestration
                        .as_ref()
                        .is_some_and(|orchestration| orchestration.flow_run_id() == CANCELLED_RUN)
            }));

            let cap_runner = runner(
                &config_path,
                &daemon_paths.socket,
                &cap_script,
                CAP_REPLAY_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            wait_for_flow_state(&client, CAP_REPLAY_RUN, 1, "running").await;
            let cap_cancelled = client
                .call("queue.cancel", Some(json!({"flowRunId": CAP_REPLAY_RUN})))
                .await
                .unwrap();
            assert_eq!(cap_cancelled["affected"], 1);
            let cap_runner = runner_output(cap_runner).await;
            assert!(
                cap_runner.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&cap_runner.stdout),
                String::from_utf8_lossy(&cap_runner.stderr)
            );
            assert_eq!(
                flow_report(&cap_runner)["report"]["finalValue"],
                json!(["cancelled", "pass"])
            );
            assert_eq!(flow_items(&client, CAP_REPLAY_RUN).await.len(), 2);
            let cap_replay = runner(
                &config_path,
                &daemon_paths.socket,
                &cap_script,
                CAP_REPLAY_RUN,
                "{}",
                1,
            )
            .spawn()
            .unwrap();
            let cap_replay = runner_output(cap_replay).await;
            assert!(
                cap_replay.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&cap_replay.stdout),
                String::from_utf8_lossy(&cap_replay.stderr)
            );
            assert_eq!(flow_items(&client, CAP_REPLAY_RUN).await.len(), 2);

            let partial = runner(
                &config_path,
                &daemon_paths.socket,
                &partial_script,
                PARTIAL_FAILURE_RUN,
                "{}",
                2,
            )
            .spawn()
            .unwrap();
            let partial = runner_output(partial).await;
            assert_eq!(partial.status.code(), Some(1));
            assert_eq!(flow_failure(&partial)["error"]["code"], "aggregate-failure");
            let partial_items = wait_for_flow_items(&client, PARTIAL_FAILURE_RUN, 2).await;
            let mut verdicts = Vec::new();
            for item in partial_items {
                let terminal = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": item["anchor"]})),
                    )
                    .await
                    .unwrap();
                verdicts.push(terminal["verdict"].as_str().unwrap().to_owned());
            }
            verdicts.sort();
            assert_eq!(verdicts, ["failed", "pass"]);

            let ordered = runner(
                &config_path,
                &daemon_paths.socket,
                &reordered_script,
                REORDERED_RUN,
                r#"{"reverse":false}"#,
                2,
            )
            .spawn()
            .unwrap();
            let ordered = runner_output(ordered).await;
            assert!(
                ordered.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&ordered.stdout),
                String::from_utf8_lossy(&ordered.stderr)
            );
            let reordered = runner(
                &config_path,
                &daemon_paths.socket,
                &reordered_script,
                REORDERED_RUN,
                r#"{"reverse":true}"#,
                2,
            )
            .spawn()
            .unwrap();
            let reordered = runner_output(reordered).await;
            assert_eq!(reordered.status.code(), Some(20));
            assert_eq!(
                flow_failure(&reordered)["error"]["code"],
                "args-changed-mid-run"
            );
            assert_eq!(flow_items(&client, REORDERED_RUN).await.len(), 2);

            let mut catalog_first = runner(
                &config_path,
                &daemon_paths.socket,
                &catalog_script,
                CATALOG_PIN_RUN,
                "{}",
                1,
            );
            catalog_first.arg("--catalog").arg(&catalog_path);
            let catalog_first = runner_output(catalog_first.spawn().unwrap()).await;
            assert!(
                catalog_first.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&catalog_first.stdout),
                String::from_utf8_lossy(&catalog_first.stderr)
            );
            fs::write(
                &catalog_path,
                "{\n  \"version\": 1,\n  \"members\": []\n}\n",
            )
            .unwrap();
            let mut catalog_changed = runner(
                &config_path,
                &daemon_paths.socket,
                &catalog_script,
                CATALOG_PIN_RUN,
                "{}",
                1,
            );
            catalog_changed.arg("--catalog").arg(&catalog_path);
            let catalog_changed = runner_output(catalog_changed.spawn().unwrap()).await;
            assert_eq!(catalog_changed.status.code(), Some(20));
            assert_eq!(
                flow_failure(&catalog_changed)["error"]["code"],
                "catalog-changed-mid-run"
            );
            assert_eq!(flow_items(&client, CATALOG_PIN_RUN).await.len(), 1);

            daemon.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn spec_build_campaign_reconciles_local_state_across_parallel_fresh_runs() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let checkout = temp.path().join("checkout");
            let remote = temp.path().join("remote.git");
            let control = temp.path().join("control");
            let workspace_root = temp.path().join("workspaces");
            fs::create_dir_all(&control).unwrap();

            copy_fixture_tree(
                &repository_fixture("test/fixtures/spec-build/repo"),
                &checkout,
            );
            fixture_git(
                temp.path(),
                &["init", "--bare", "--initial-branch=main", "remote.git"],
            );
            fixture_git(&checkout, &["init", "--initial-branch=main"]);
            fixture_git(&checkout, &["config", "user.name", "Tally Fixture"]);
            fixture_git(
                &checkout,
                &["config", "user.email", "tally-fixture@invalid"],
            );
            fixture_git(&checkout, &["add", "."]);
            fixture_git(&checkout, &["commit", "-m", "fixture: frozen spec"]);
            fixture_git(
                &checkout,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );
            fixture_git(&checkout, &["push", "--set-upstream", "origin", "main"]);

            let driver = rust_spec_build_driver();
            let agent = repository_fixture("test/fixtures/spec-build/policy-agent.py");

            let mut config = config();
            for (name, resource, capacity) in [
                ("campaign-control", ResourceKind::CpuSlot, 4),
                ("campaign-agent", ResourceKind::Slot, 3),
            ] {
                config.pools.insert(
                    name.to_owned(),
                    PoolConfig {
                        resource: Some(resource),
                        capacity,
                        predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                        ..PoolConfig::default()
                    },
                );
            }
            config.adapters.insert(
                "spec-build-driver".to_owned(),
                AdapterConfig {
                    scrape: BTreeMap::from([(
                        "finalMessage".to_owned(),
                        ScrapeCapture {
                            stream: ScrapeStream::Stdout,
                            mode: ScrapeMode::Regex,
                            pattern: "^TALLY_FINAL_MESSAGE=(.*)$".to_owned(),
                            counter_scope: None,
                            fields: Default::default(),
                        },
                    )]),
                    ..AdapterConfig::default()
                },
            );
            config.adapters.insert(
                "codex".to_owned(),
                AdapterConfig {
                    argv: vec![
                        "python3".to_owned(),
                        agent.display().to_string(),
                        control.display().to_string(),
                        "--".to_owned(),
                    ],
                    scrape: BTreeMap::from([(
                        "finalMessage".to_owned(),
                        ScrapeCapture {
                            stream: ScrapeStream::Stdout,
                            mode: ScrapeMode::Regex,
                            pattern: "^TALLY_FINAL_MESSAGE=(.*)$".to_owned(),
                            counter_scope: None,
                            fields: Default::default(),
                        },
                    )]),
                    launch: AdapterLaunchConfig {
                        cwd_argv: Some(vec!["-C".to_owned(), "%<cwd>%".to_owned()]),
                        approval_policies: BTreeMap::from([(
                            "never".to_owned(),
                            vec!["-c".to_owned(), "approval_policy=\"never\"".to_owned()],
                        )]),
                        sandbox_policies: BTreeMap::from([
                            (
                                "danger-full-access".to_owned(),
                                vec!["--sandbox".to_owned(), "danger-full-access".to_owned()],
                            ),
                            (
                                "read-only".to_owned(),
                                vec!["--sandbox".to_owned(), "read-only".to_owned()],
                            ),
                        ]),
                        commit_capable_sandbox_policies: BTreeSet::from([
                            "danger-full-access".to_owned()
                        ]),
                        // A model override the adapter authorizes. The
                        // campaign dispatches with it, the daemon records it as
                        // the job's canonical model, and the merge node names
                        // that -- and only that -- in its trailer.
                        model: Some(AdapterValueOverride {
                            argv: vec!["--model".to_owned(), "%<value>%".to_owned()],
                            allowed_values: vec!["fixture/policy-agent-1".to_owned()],
                        }),
                        ..AdapterLaunchConfig::default()
                    },
                    ..AdapterConfig::default()
                },
            );
            config.validate().unwrap();
            let config_path = temp.path().join("config.json");
            fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
            let configured_tally = temp.path().join("configured-tally");
            shell_program::install(
                &configured_tally,
                format!(
                    "#!/bin/sh\nexec '{}' --config '{}' \"$@\"\n",
                    env!("CARGO_BIN_EXE_tally"),
                    config_path.display()
                ),
            );

            let daemon_paths = paths(&temp.path().join("daemon"));
            let daemon = start_daemon(&daemon_paths, config).await;
            let client = rpc(&daemon_paths.socket).await;
            let script = repository_fixture("examples/flows/spec-build.js");
            let gate = repository_fixture("test/fixtures/spec-build/gate.sh");
            let preflight = repository_fixture("test/fixtures/spec-build/preflight.sh");
            let first_preflight_argv = vec![
                "/bin/sh".to_owned(),
                preflight.display().to_string(),
                control.display().to_string(),
                "first".to_owned(),
            ];
            let second_preflight_argv = vec![
                "/bin/sh".to_owned(),
                preflight.display().to_string(),
                control.display().to_string(),
                "second".to_owned(),
            ];
            let first_gate_argv = vec![
                "/bin/sh".to_owned(),
                gate.display().to_string(),
                control.display().to_string(),
                "first".to_owned(),
            ];
            let second_gate_argv = vec![
                "/bin/sh".to_owned(),
                gate.display().to_string(),
                control.display().to_string(),
                "second".to_owned(),
            ];
            // The pass writes its own successor here instead of posting a
            // public `/tally reconcile` comment. A scratch directory keeps the
            // fixture hermetic: nothing drains it, so the file itself is the
            // observation.
            let continuation_events = temp.path().join("continuation-events");
            let attempt_receipts_path = daemon_paths
                .state_dir
                .join("campaigns/attempt-receipts/fixture/attempt-receipts-v1.jsonl");
            let write_receipt_authority = |arm_serial: u64| {
                let worklist = fs::read(checkout.join("specs/001-toy/tasks.json")).unwrap();
                let worklist_sha256 = format!("sha256:{:x}", Sha256::digest(worklist));
                let authority_path = attempt_receipts_path
                    .parent()
                    .unwrap()
                    .join("receipt-authority-v1.json");
                fs::create_dir_all(authority_path.parent().unwrap()).unwrap();
                fs::write(
                    &authority_path,
                    serde_json::to_vec(&json!({
                        "schemaVersion": 1,
                        "campaign": "fixture",
                        "issueNumber": "7",
                        "armSerial": arm_serial,
                        "worklistSha256": &worklist_sha256,
                    }))
                    .unwrap(),
                )
                .unwrap();
                fs::set_permissions(&authority_path, fs::Permissions::from_mode(0o600)).unwrap();
                worklist_sha256
            };
            let initial_worklist_sha256 = write_receipt_authority(1);
            let integration_branch = "tally/fixture-campaign-fixture/integration";
            let arguments = |run_id: &str, priority: &str| {
                json!({
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "issue": {
                        "number": "7",
                        "url": "local://acme/spec/specs/*/tasks.json"
                    },
                    "runId": run_id,
                    "repositories": {
                        "acme/spec": {
                            "checkout": checkout,
                            "baseBranch": "main",
                            "remote": "origin",
                            "forge": "local"
                        }
                    },
                    "worklist": "specs/*/tasks.json",
                    "maxTasks": 7,
                    "maxParallel": 3,
                    "continuation": {
                        "argv": [
                            env!("CARGO_BIN_EXE_tally"),
                            "--config",
                            config_path,
                            "flow",
                            "run",
                            script,
                            "--args-from-brief",
                            "--max-nodes",
                            "55"
                        ],
                        "pool": ["flow", "fixture-campaign"],
                        "priority": "low",
                        "runtimeMaxSec": 600,
                        "eventsDir": continuation_events
                    },
                    "workspaceRoot": workspace_root,
                    "captureRoot": daemon_paths.state_dir.join("capture/archive"),
                    "postFailureEvidence": false,
                    "postFailureStderr": false,
                    "tally": configured_tally,
                    "driver": driver,
                    "driverRuntimeMaxSec": 30,
                    "agent": {
                        "adapter": "codex",
                        "argv": [BRIEF_SENTINEL],
                        "model": "fixture/policy-agent-1",
                        "priority": priority,
                        "runtimeMaxSec": 30,
                        // Every policy is spelled out, and the fixture agent
                        // below asserts each one reaches the launch argv: this
                        // is the live witness that an explicit worklist value
                        // wins outright over whatever the adapter declares for
                        // a campaign that names none.
                        "approvalPolicy": "never",
                        "sandboxPolicy": "danger-full-access",
                        "diagnosisSandboxPolicy": "read-only"
                    },
                    "gates": [
                        {
                            "kind": "forbidPaths",
                            "id": "no-db-artifacts",
                            "forbidPaths": ["*.db", "*.db-wal", "*.db-shm", "*.sqlite*"],
                            "runtimeMaxSec": 1
                        },
                        {
                            "kind": "command",
                            "id": "fixture-first",
                            "preflightArgv": first_preflight_argv.clone(),
                            "argv": first_gate_argv.clone(),
                            "runtimeMaxSec": 1
                        },
                        {
                            "kind": "command",
                            "id": "fixture-second",
                            "preflightArgv": second_preflight_argv.clone(),
                            "argv": second_gate_argv.clone(),
                            "runtimeMaxSec": 1
                        }
                    ]
                })
                .to_string()
            };

            let mut duplicate_arguments: Value =
                serde_json::from_str(&arguments("duplicate-gate", "low")).unwrap();
            duplicate_arguments["gates"][2]["id"] = json!("fixture-first");
            let duplicate = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_DUPLICATE_GATE_RUN,
                &duplicate_arguments.to_string(),
                20,
            )
            .spawn()
            .unwrap();
            let duplicate = runner_output(duplicate).await;
            assert_eq!(
                duplicate.status.code(),
                Some(1),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&duplicate.stdout),
                String::from_utf8_lossy(&duplicate.stderr)
            );
            let duplicate_failure = flow_failure(&duplicate);
            assert_eq!(
                duplicate_failure["error"]["name"],
                "SpecBuildConfigurationError"
            );
            assert_eq!(duplicate_failure["error"]["code"], "duplicate-gate-id");
            assert!(duplicate_failure["error"]["message"]
                .as_str()
                .unwrap()
                .contains("campaign gate id fixture-first is duplicated"));
            assert!(
                flow_items(&client, SPEC_BUILD_DUPLICATE_GATE_RUN)
                    .await
                    .is_empty(),
                "duplicate gate ids must be rejected before reconciliation is admitted"
            );

            // The ordinary red preflight: a plain non-zero exit inside the
            // deadline -- the "this host has no toolchain" shape. It must reach
            // the same `preflight-failed` refusal as the timeout below, admit
            // no agent, and leave no lane behind. A gate whose own base-safe
            // probe is red is never witnessed, so the pass stops at five nodes.
            let red_preflight = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RED_PREFLIGHT_RUN,
                &arguments("red-preflight-comment", "low"),
                20,
            )
            .spawn()
            .unwrap();
            let red_preflight = runner_output(red_preflight).await;
            assert_eq!(
                red_preflight.status.code(),
                Some(1),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&red_preflight.stdout),
                String::from_utf8_lossy(&red_preflight.stderr)
            );
            let red_failure = flow_failure(&red_preflight);
            assert_eq!(red_failure["error"]["name"], "SpecBuildPreflightError");
            assert_eq!(red_failure["error"]["code"], "preflight-failed");
            assert_eq!(red_failure["error"]["details"]["gateId"], "fixture-first");
            assert_eq!(
                red_failure["error"]["details"]["preflightArgv"],
                json!(first_preflight_argv),
                "the witnessed failure record must preserve the failed argv verbatim"
            );
            assert_eq!(
                red_failure["error"]["details"]["node"]["taskRef"],
                "fixture/task-1"
            );
            let rendered_preflight_argv =
                serde_json::to_string(&first_preflight_argv).unwrap();
            assert!(
                red_failure["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(&rendered_preflight_argv),
                "preflight failure message omitted argv {rendered_preflight_argv}: {}",
                red_failure["error"]["message"]
            );
            assert_eq!(
                red_failure["error"]["details"]["node"]["verdict"],
                "failed",
                "a plain non-zero preflight exit is not a runtime-exceeded verdict"
            );
            assert_eq!(
                red_failure["error"]["details"]["node"]["stderrExcerpt"],
                "fixture preflight cannot find the toolchain on this host\n"
            );
            let red_terminal = runner_events(&red_preflight, "node-terminal")
                .into_iter()
                .find(|event| event["verdict"] == "failed")
                .expect("runner omitted the failed node-terminal event");
            assert_eq!(
                red_terminal["stderrExcerpt"],
                "fixture preflight cannot find the toolchain on this host\n"
            );
            let red_items =
                wait_for_flow_items(&client, SPEC_BUILD_RED_PREFLIGHT_RUN, 5).await;
            assert_eq!(
                red_items.len(),
                5,
                "a red gating preflight must admit only sweep, reconcile, prep, gate, \
                 and cleanup: {:?}",
                red_items
                    .iter()
                    .map(|item| item["orchestration"]["nodeLabel"]
                        .as_str()
                        .unwrap_or("<missing>"))
                    .collect::<Vec<_>>()
            );
            for item in &red_items {
                client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": item["anchor"]})),
                    )
                    .await
                    .unwrap();
            }
            assert!(!control.join("agent-order.log").exists());
            assert!(!control.join("gate-order.log").exists());
            assert!(!control.join("preflight-order.log").exists());
            assert!(
                !fixture_git(&checkout, &["worktree", "list", "--porcelain"])
                    .contains("_campaign-preflight")
            );

            let first = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN,
                &arguments("fixture-comment-7", "low"),
                48,
            )
            .spawn()
            .unwrap();
            let first = runner_output(first).await;
            assert_eq!(first.status.code(), Some(1));
            let failure = flow_failure(&first);
            assert_eq!(failure["error"]["code"], "preflight-failed");
            assert_eq!(failure["error"]["details"]["gateId"], "fixture-first");
            assert_eq!(
                failure["error"]["details"]["node"]["stderrExcerpt"],
                "fixture preflight exceeds its bounded deadline before any agent dispatch\n"
            );
            assert_eq!(
                failure["error"]["details"]["node"]["stderrTruncated"],
                false
            );
            assert!(
                String::from_utf8_lossy(&first.stderr).contains(
                    "fixture preflight exceeds its bounded deadline before any agent dispatch"
                ),
                "runner stderr omitted the child diagnostic: {}",
                String::from_utf8_lossy(&first.stderr)
            );
            let terminal = runner_events(&first, "node-terminal")
                .into_iter()
                .find(|event| event["verdict"] == "runtime-exceeded")
                .expect("runner omitted the failed node-terminal event");
            assert_eq!(
                terminal["stderrExcerpt"],
                "fixture preflight exceeds its bounded deadline before any agent dispatch\n"
            );
            assert!(
                !control.join("policy-error.log").exists(),
                "{}",
                fs::read_to_string(control.join("policy-error.log")).unwrap_or_default()
            );

            let first_items = wait_for_flow_items(&client, SPEC_BUILD_RUN, 5).await;
            assert_eq!(
                first_items.len(),
                5,
                "a red preflight must admit only sweep, reconcile, prep, gate, and cleanup"
            );
            assert!(first_items[..2]
                .iter()
                .all(|item| item.get("taskRef").is_none()));
            assert!(first_items[2..]
                .iter()
                .all(|item| item["taskRef"] == "fixture/task-1"));
            let mut failed_preflight = None;
            for item in &first_items {
                let terminal = client
                    .call(
                        "queue.await_job",
                        Some(json!({"task_uuid": item["anchor"]})),
                    )
                    .await
                    .unwrap();
                let projected = client
                    .call("query.job", Some(json!({"id": item["anchor"]})))
                    .await
                    .unwrap();
                if projected["job"]["orchestration"]["nodeLabel"] == "preflight-gate-fixture-first"
                {
                    assert_ne!(terminal["verdict"], "pass");
                    failed_preflight = terminal["task_uuid"].as_str().map(str::to_owned);
                } else {
                    assert_eq!(terminal["verdict"], "pass");
                }
            }
            let failed_preflight = failed_preflight.expect("the preflight gate did not fail");
            let projected_preflight = client
                .call("query.job", Some(json!({"id": failed_preflight.clone()})))
                .await
                .unwrap();
            assert_eq!(projected_preflight["job"]["taskRef"], "fixture/task-1");
            assert_eq!(
                projected_preflight["job"]["unit"],
                format!("tally-job-fixture-task-1-{failed_preflight}.service")
            );
            let failed_preflight_capture = daemon_paths
                .state_dir
                .join(format!("capture/{failed_preflight}.task-1.err"));
            assert!(failed_preflight_capture.is_file());
            assert_eq!(
                projected_preflight["job"]["argv"],
                json!(first_preflight_argv),
                "preflight must execute the declared base-safe argv without rewriting it"
            );
            assert_eq!(
                projected_preflight["job"]["orchestration"]["nodeLabel"],
                "preflight-gate-fixture-first"
            );
            assert_eq!(projected_preflight["job"]["runtimeMaxSec"], 1);
            assert!(
                task_capture(&daemon_paths, &failed_preflight, "task-1").contains(
                    "fixture preflight exceeds its bounded deadline before any agent dispatch"
                )
            );
            assert!(!control.join("agent-order.log").exists());
            assert!(!control.join("policy-order.log").exists());
            assert!(!control.join("preflight-order.log").exists());
            assert!(!control.join("gate-order.log").exists());
            assert_eq!(
                fixture_git(&checkout, &["rev-list", "--count", "origin/main"]),
                "1",
                "preflight failure must stop before every implementation lane"
            );
            assert!(
                !fixture_git(&checkout, &["worktree", "list", "--porcelain"])
                    .contains("_campaign-preflight")
            );

            fs::write(control.join("hold-task-1"), b"").unwrap();
            fs::write(control.join("hold-task-3"), b"").unwrap();
            let mut orphan = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_ORPHAN_RUN,
                &arguments("orphan-comment", "low"),
                32,
            )
            .spawn()
            .unwrap();
            wait_for_path(&control.join("holding-task-1")).await;
            wait_for_path(&control.join("holding-task-3")).await;
            let orphan_items = flow_items(&client, SPEC_BUILD_ORPHAN_RUN).await;
            assert!(orphan_items.iter().any(|item| {
                item["orchestration"]["nodeLabel"] == "agent-task-1"
                    && item["liveState"] == "running"
            }));
            assert!(orphan_items.iter().any(|item| {
                item["orchestration"]["nodeLabel"] == "agent-task-3"
                    && item["liveState"] == "running"
            }));

            orphan.kill().await.unwrap();
            let orphan_status = orphan.wait().await.unwrap();
            assert!(!orphan_status.success());

            let deferred = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_DEFERRED_RUN,
                &arguments("deferred-comment", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let deferred = runner_output(deferred).await;
            assert!(
                deferred.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&deferred.stdout),
                String::from_utf8_lossy(&deferred.stderr)
            );
            let deferred_value = &flow_report(&deferred)["report"]["finalValue"];
            assert_eq!(deferred_value["state"], "deferred-live-jobs");
            assert!(deferred_value["maintenance"]["blockingJobs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|job| {
                    job["flowRunId"] == SPEC_BUILD_ORPHAN_RUN
                        && job["taskRef"] == "fixture/task-1"
                        && job["liveState"] == "running"
                }));
            assert!(deferred_value["maintenance"]["liveRuns"]
                .as_array()
                .unwrap()
                .iter()
                .any(|run| run["flowRunId"] == SPEC_BUILD_ORPHAN_RUN));
            let deferred_items = wait_for_flow_items(&client, SPEC_BUILD_DEFERRED_RUN, 1).await;
            assert_eq!(
                deferred_items[0]["orchestration"]["nodeLabel"],
                "spec-build-sweep"
            );
            assert!(
                fixture_git(&checkout, &["worktree", "list", "--porcelain"])
                    .contains(workspace_root.to_str().unwrap()),
                "a fresh pass removed a lane still owned by live daemon jobs"
            );

            fs::remove_file(control.join("hold-task-1")).unwrap();
            fs::remove_file(control.join("hold-task-3")).unwrap();
            await_items(&client, &orphan_items).await;
            for artifact in [
                "agent-order.log",
                "policy-order.log",
                "preflight-order.log",
                "started-task-1",
                "started-task-3",
                "holding-task-1",
                "holding-task-3",
            ] {
                let path = control.join(artifact);
                if path.exists() {
                    fs::remove_file(path).unwrap();
                }
            }

            fs::write(control.join("inject-forbidden-path"), b"").unwrap();
            fs::write(control.join("post-change-failed-once"), b"").unwrap();

            let second = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_2,
                &arguments("fixture-comment-8", "medium"),
                48,
            )
            .spawn()
            .unwrap();
            let second = runner_output(second).await;
            assert!(
                second.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&second.stdout),
                String::from_utf8_lossy(&second.stderr)
            );
            let second_value = &flow_report(&second)["report"]["finalValue"];
            assert_eq!(second_value["state"], "advanced");
            assert_eq!(
                second_value["reconciled"]["frontier"],
                json!(["task-1", "task-3"])
            );
            assert_eq!(second_value["merged"][0]["taskId"], "task-3");
            // The campaign default is squash, so the local integration branch
            // must carry a single-parent commit per merged task, not a merge
            // commit. A `--no-ff` merge would show two parents here.
            fixture_git(&checkout, &["fetch", "--prune", "origin"]);
            let parents = fixture_git(
                &checkout,
                &["log", "--format=%P", integration_branch],
            );
            assert!(
                parents
                    .lines()
                    .all(|line| line.split_whitespace().count() <= 1),
                "squash merges must leave no merge commit on integration:\n{parents}"
            );
            // The receipt is a local audit index. Completion itself is the
            // exact task/revision trailer pair on first-parent integration
            // history.
            let receipts = fixture_git(
                &checkout,
                &[
                    "for-each-ref",
                    "--format=%(objectname) %(refname)",
                    "refs/tally/spec-build/v1/",
                ],
            );
            assert!(
                receipts.contains("/merge/task-3"),
                "squash merge must record a local task-3 receipt ref:\n{receipts}"
            );
            let task_3_merge = second_value["merged"][0]["mergeCommit"]
                .as_str()
                .unwrap();
            assert!(receipts.lines().any(|line| {
                line.starts_with(task_3_merge) && line.contains("/merge/task-3")
            }));
            let task_3_trailers = fixture_git(
                &checkout,
                &[
                    "log",
                    "-1",
                    "--format=%(trailers:only,unfold=true)",
                    task_3_merge,
                ],
            );
            assert!(
                task_3_trailers.starts_with("Tally-Task: task-3\nTally-Revision: sha256:"),
                "completion trailers must lead one contiguous trailer block:\n{task_3_trailers}"
            );
            // With no steward configured the narration is the brief-derived
            // template, and it is what the squash commit says.
            assert_eq!(
                fixture_git(
                    &checkout,
                    &["log", "-1", "--format=%s", integration_branch],
                ),
                "task-3: Create an independent fixture artifact"
            );
            // §7's provenance pointer is the node's own, assembled from the
            // witnessed implementation attempt: the adapter, the canonical
            // model the daemon recorded, the task UUID, and the witness
            // sequence. It is byte-identical to what the gh producer
            // publishes, and the narrator is refused if it proposes one.
            let trailer = second_value["merged"][0]["trailer"].as_str().unwrap();
            assert!(
                trailer.starts_with("Assisted-by: codex:fixture/policy-agent-1 (tally:"),
                "{trailer}"
            );
            assert!(trailer.ends_with(')'), "{trailer}");
            assert!(trailer.contains(" witness:"), "{trailer}");
            assert!(
                task_3_trailers.ends_with(trailer),
                "provenance must share the completion trailer block:\n{task_3_trailers}"
            );
            let message = fixture_git(
                &checkout,
                &["log", "-1", "--format=%B", integration_branch],
            );
            assert!(
                message.contains(trailer),
                "the squash message must carry the trailer:\n{message}"
            );
            assert_eq!(
                fixture_git(&checkout, &["rev-list", "--count", "origin/main"]),
                "1",
                "task integration must not advance the shared remote base"
            );
            assert_eq!(second_value["failures"][0]["taskId"], "task-1");
            assert_eq!(
                second_value["failures"][0]["stage"],
                "gate:no-db-artifacts"
            );
            assert_eq!(second_value["diagnoses"][0]["taskId"], "task-1");
            assert_eq!(second_value["diagnoses"][0]["attempt"], 1);
            assert_eq!(second_value["diagnoses"][0]["blocked"], false);
            assert_eq!(second_value["diagnoses"][0]["redacted"], true);
            assert_eq!(second_value["continuation"]["created"], true);
            assert_eq!(
                second_value["continuation"]["dedupKey"],
                format!(
                    "campaign-continuation:acme/spec:7:{}",
                    second_value["continuation"]["runId"].as_str().unwrap()
                )
            );
            let second_continuation = PathBuf::from(
                second_value["continuation"]["event"].as_str().unwrap(),
            );
            assert!(second_continuation.starts_with(&continuation_events));
            let second_event: Value =
                serde_json::from_slice(&fs::read(&second_continuation).unwrap()).unwrap();
            assert_eq!(second_event["source"], "events-dir");
            assert_eq!(second_event["adapter"], "shell");
            assert_eq!(second_event["pool"], json!(["flow", "fixture-campaign"]));
            assert_eq!(
                second_event["dedupKey"],
                second_value["continuation"]["dedupKey"]
            );
            assert_eq!(
                second_event["brief"]["runId"],
                second_value["continuation"]["runId"],
                "the next pass carries the derived continuation identity"
            );
            assert_eq!(second_event["brief"]["campaign"], "fixture");
            assert_eq!(second_event["argv"][1], "--config");
            assert_eq!(
                second_event["argv"][2],
                config_path.to_string_lossy().as_ref(),
                "the continuation re-enters through the same explicit config"
            );
            assert_eq!(
                second_event["argv"][3], "flow",
                "a module-declared campaign re-enters through its own flow-run argv"
            );
            assert!(control.join("started-task-1").exists());
            assert!(control.join("started-task-3").exists());
            let second_submitted = runner_events(&second, "node-submitted");
            assert!(second_submitted
                .iter()
                .all(|event| event["disposition"] == "created"));
            assert!(second_submitted
                .iter()
                .any(|event| event["label"] == "ownership-task-3"));

            assert_eq!(
                second_submitted.len(),
                // task-1's declared domains admit the transient database so
                // the tree-delta check passes and `no-db-artifacts` itself is
                // the witnessed failure. Both nodes now run before diagnosis.
                29,
                "unexpected second-pass nodes: {:?}",
                second_submitted
                    .iter()
                    .map(|event| event["label"].as_str().unwrap_or("<missing>"))
                    .collect::<Vec<_>>()
            );
            let second_items = wait_for_flow_items(&client, SPEC_BUILD_RUN_2, 29).await;
            assert_eq!(
                second_items.len(),
                29,
                "unexpected durable second-pass nodes: {:?}",
                json!({
                    "durable": second_items
                        .iter()
                        .map(|item| {
                            item["orchestration"]["nodeLabel"]
                                .as_str()
                                .unwrap_or("<missing>")
                        })
                        .collect::<Vec<_>>(),
                    "submitted": second_submitted,
                    "report": second_value,
                })
            );
            // Four preflight nodes became six: prep, cleanup, and now a
            // gating probe plus a non-gating real-argv witness for each of the
            // two command gates -- all carrying the first frontier
            // implementation's taskRef, on top of task-1's own nine lane
            // nodes through its failing forbidPaths check.
            assert_eq!(
                second_submitted
                    .iter()
                    .filter(|event| event["taskRef"] == "fixture/task-1")
                    .count(),
                15
            );
            assert_eq!(
                second_submitted
                    .iter()
                    .filter(|event| event["taskRef"] == "fixture/task-3")
                    .count(),
                // #386: `tree-delta-task-3` carries task-3's own taskRef.
                11
            );
            assert_eq!(
                second_submitted
                    .iter()
                    .filter(|event| event.get("taskRef").is_none())
                    .count(),
                3,
                "sweep, reconcile, and one end-of-pass continuation have no task ref"
            );
            assert_eq!(
                second_items
                    .iter()
                    .filter(|item| item["taskRef"] == "fixture/task-1")
                    .count(),
                15
            );
            assert_eq!(
                second_items
                    .iter()
                    .filter(|item| item["taskRef"] == "fixture/task-3")
                    .count(),
                // #386: `tree-delta-task-3` carries task-3's own taskRef.
                11
            );
            let mut failed_constraint = None;
            for item in &second_items {
                let projected = client
                    .call("query.job", Some(json!({"id": item["anchor"]})))
                    .await
                    .unwrap();
                if projected["job"]["orchestration"]["nodeLabel"]
                    == "gate-task-1-no-db-artifacts"
                {
                    failed_constraint = item["anchor"].as_str().map(str::to_owned);
                    break;
                }
            }
            let failed_constraint =
                failed_constraint.expect("the forbidPaths gate was not projected");
            assert_eq!(
                second_items
                    .iter()
                    .filter(|item| item.get("taskRef").is_none())
                    .count(),
                3
            );
            let projected_constraint = client
                .call("query.job", Some(json!({"id": failed_constraint.clone()})))
                .await
                .unwrap();
            assert_eq!(projected_constraint["job"]["taskRef"], "fixture/task-1");
            assert_eq!(projected_constraint["job"]["runtimeMaxSec"], 1);
            assert!(task_capture(&daemon_paths, &failed_constraint, "task-1")
                .contains("build/transient.db"));
            assert_eq!(
                fs::read_to_string(control.join("preflight-order.log")).unwrap(),
                "preflight:task-1:first\npreflight:task-1:second\n"
            );

            // #320: once every base-safe probe was green the pass also ran each
            // gate's real merge-criterion argv once, on the same lane, as a
            // non-gating witness. Both are red there -- the fixture gate needs a
            // `build` directory no agent has created yet -- and the pass still
            // dispatched both frontier agents and merged task-3 above. The exit
            // code and stderr are the evidence #264's split left unavailable at
            // t=0.
            //
            // The two phases must not interleave. A probe is declared base-safe
            // and this fixture's probe asserts the pristine premise
            // (`test ! -e preflight-witness-ran`, preflight.sh); the witness
            // writes that marker, because a gate's real argv is the merge
            // criterion and is expected to build and write. So a witness running
            // between two probes turns the second gate's probe red and refuses
            // admission naming the innocent gate. Asserting the submission order
            // states the invariant directly; the fixture pair is the tripwire
            // that fires if it is ever broken.
            let preflight_labels = second_submitted
                .iter()
                .filter_map(|event| event["label"].as_str())
                .filter(|label| label.starts_with("preflight-gate-")
                    || label.starts_with("preflight-witness-"))
                .collect::<Vec<_>>();
            assert_eq!(
                preflight_labels,
                vec![
                    "preflight-gate-fixture-first",
                    "preflight-gate-fixture-second",
                    "preflight-witness-fixture-first",
                    "preflight-witness-fixture-second",
                ],
                "every gating probe must run on the pristine base before any witness mutates it"
            );
            let second_terminals = runner_events(&second, "node-terminal");
            for (label, argv) in [
                ("preflight-witness-fixture-first", &first_gate_argv),
                ("preflight-witness-fixture-second", &second_gate_argv),
            ] {
                let submitted = second_submitted
                    .iter()
                    .find(|event| event["label"] == label)
                    .unwrap_or_else(|| panic!("the pass never submitted {label}"));
                assert_eq!(submitted["taskRef"], "fixture/task-1", "{label}");
                let uuid = submitted["taskUuid"].as_str().unwrap();
                let terminal = second_terminals
                    .iter()
                    .find(|event| event["taskUuid"] == uuid)
                    .unwrap_or_else(|| panic!("{label} never reached a terminal verdict"));
                assert_eq!(terminal["verdict"], "failed", "{label}");
                assert_eq!(terminal["exitCode"], 3, "{label}");
                assert_eq!(
                    terminal["stderrExcerpt"],
                    "fixture gate argv is red on the pristine campaign base\n",
                    "{label}"
                );
                let projected = client
                    .call("query.job", Some(json!({"id": uuid})))
                    .await
                    .unwrap();
                assert_eq!(
                    projected["job"]["argv"],
                    json!(argv),
                    "{label} must execute the gate's real argv without rewriting it"
                );
                assert_eq!(projected["job"]["taskRef"], "fixture/task-1", "{label}");
                assert_eq!(projected["job"]["runtimeMaxSec"], 1, "{label}");
                assert!(
                    task_capture(&daemon_paths, uuid, "task-1")
                        .contains("fixture gate argv is red on the pristine campaign base"),
                    "{label} did not retain its capture"
                );
            }

            assert_eq!(
                fixture_git(
                    &checkout,
                    &["show", &format!("{integration_branch}:build/three.txt")],
                ),
                "three"
            );
            assert!(
                StdCommand::new("git")
                    .arg("-C")
                    .arg(&checkout)
                    .args([
                        "cat-file",
                        "-e",
                        &format!("{integration_branch}:build/one.txt"),
                    ])
                    .status()
                    .unwrap()
                    .code()
                    .is_some_and(|code| code != 0),
                "the failed task must remain unmerged"
            );
            assert!(
                !fixture_git(&checkout, &["worktree", "list", "--porcelain"])
                    .contains(workspace_root.to_str().unwrap()),
                "pass exit must reclaim both the failed and merged frontier lanes"
            );
            let state_root = workspace_root.join(".state");
            assert!(
                !state_root.exists()
                    || fs::read_dir(&state_root).unwrap().all(|entry| {
                        matches!(
                            entry.unwrap().file_name().to_str(),
                            Some("passes" | "sweep.lock")
                        )
                    }),
                "pass exit left task prep state behind"
            );

            let third = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_3,
                &arguments("fixture-comment-9", "medium"),
                48,
            )
            .spawn()
            .unwrap();
            let third = runner_output(third).await;
            assert!(
                third.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&third.stdout),
                String::from_utf8_lossy(&third.stderr)
            );
            let third_value = &flow_report(&third)["report"]["finalValue"];
            assert_eq!(third_value["reconciled"]["frontier"], json!(["task-1"]));
            assert_eq!(third_value["reconciled"]["diagnoses"][0]["attempt"], 1);
            assert_eq!(third_value["merged"][0]["taskId"], "task-1");
            assert_eq!(third_value["failures"], json!([]));
            assert_eq!(third_value["diagnoses"], json!([]));
            assert!(control.join("task-1-steering-visible").exists());
            let third_submitted = runner_events(&third, "node-submitted");
            // #386: `tree-delta-task-1` is a new node in the chain now that
            // task-1's ownership passes and it reaches the gate.
            assert_eq!(third_submitted.len(), 14);
            assert_eq!(
                third_submitted
                    .iter()
                    .filter(|event| event["taskRef"] == "fixture/task-1")
                    .count(),
                11
            );
            assert_eq!(
                third_submitted
                    .iter()
                    .filter(|event| event.get("taskRef").is_none())
                    .count(),
                3
            );
            let third_items = wait_for_flow_items(&client, SPEC_BUILD_RUN_3, 14).await;
            assert_eq!(
                third_items
                    .iter()
                    .filter(|item| item["taskRef"] == "fixture/task-1")
                    .count(),
                11
            );
            assert_eq!(
                third_items
                    .iter()
                    .filter(|item| item.get("taskRef").is_none())
                    .count(),
                3
            );

            assert_eq!(
                fixture_git(
                    &checkout,
                    &["show", &format!("{integration_branch}:build/one.txt")],
                ),
                "one"
            );
            assert_eq!(
                fixture_git(
                    &checkout,
                    &[
                        "show",
                        &format!("{integration_branch}:build/checkpoint-red"),
                    ],
                ),
                "pending phase validation"
            );

            let fourth = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_4,
                &arguments("fixture-comment-10-frontier", "high"),
                48,
            )
            .spawn()
            .unwrap();
            let fourth = runner_output(fourth).await;
            assert!(
                fourth.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&fourth.stdout),
                String::from_utf8_lossy(&fourth.stderr)
            );
            let fourth_value = &flow_report(&fourth)["report"]["finalValue"];
            assert_eq!(
                fourth_value["reconciled"]["frontier"],
                json!(["task-4", "task-6", "phase-one-checkpoint"]),
                "a checkpoint unrelated outstanding work can still flip is considered last"
            );
            assert_eq!(
                fourth_value["reconciled"]["deferrals"],
                json!([{
                    "taskId": "phase-one-checkpoint",
                    "waitingOn": ["task-4", "task-6"]
                }])
            );
            assert_eq!(fourth_value["merged"][0]["taskId"], "task-4");
            assert_eq!(fourth_value["merged"][1]["taskId"], "task-6");
            assert_eq!(fourth_value["merged"][1]["regated"], true);
            assert_eq!(fourth_value["checkpoints"], json!([]));
            assert_eq!(
                fourth_value["failures"][0]["taskId"],
                "phase-one-checkpoint"
            );
            assert_eq!(fourth_value["failures"][0]["stage"], "checkpoint");
            assert_eq!(
                fourth_value["diagnoses"],
                json!([]),
                "a checkpoint red only because unrelated work is outstanding spends no attempt"
            );
            assert_eq!(fourth_value["deferrals"], json!(["phase-one-checkpoint"]));
            assert_eq!(fourth_value["retries"], json!([]));
            assert_eq!(fourth_value["continuation"]["created"], true);
            let fourth_submitted = runner_events(&fourth, "node-submitted");
            // #386: `tree-delta-task-4` and `tree-delta-task-6` are new nodes
            // now that both tasks' ownership passes and each reaches the gate.
            // A red checkpoint also snapshots its output before cleanup.
            assert_eq!(fourth_submitted.len(), 32);
            assert!(fourth_submitted
                .iter()
                .all(|event| event["disposition"] == "created"));
            assert_eq!(
                fourth_submitted
                    .iter()
                    .filter(|event| event["taskRef"] == "fixture/phase-one-checkpoint")
                    .count(),
                4,
                "a deferred checkpoint prepares, runs, records, and cleans up without steering"
            );
            assert_eq!(
                fourth_submitted
                    .iter()
                    .filter(|event| event["taskRef"] == "fixture/task-4")
                    .count(),
                11
            );
            assert_eq!(
                fourth_submitted
                    .iter()
                    .filter(|event| event["taskRef"] == "fixture/task-6")
                    .count(),
                14
            );
            assert_eq!(
                fourth_submitted
                    .iter()
                    .filter(|event| event.get("taskRef").is_none())
                    .count(),
                3
            );
            assert!(fourth_submitted
                .iter()
                .any(|event| event["label"] == "regate-task-6-no-db-artifacts"));
            assert!(fourth_submitted
                .iter()
                .any(|event| event["label"] == "checkpoint-phase-one-checkpoint"));
            assert!(fourth_submitted
                .iter()
                .any(|event| event["label"] == "checkpoint-record-phase-one-checkpoint"));

            let fourth_items = wait_for_flow_items(&client, SPEC_BUILD_RUN_4, 32).await;
            let failed_checkpoint = fourth_items
                .iter()
                .find(|item| {
                    item["orchestration"]["nodeLabel"] == "checkpoint-phase-one-checkpoint"
                })
                .expect("the failed checkpoint node was not projected");
            assert_eq!(
                failed_checkpoint["argv"],
                json!([
                    "sh",
                    "-eu",
                    "-c",
                    "test \"$(cat build/one.txt)\" = one; if test -e build/checkpoint-red; then echo 'phase one checkpoint remains red' >&2; exit 1; fi; grep -q '\"attempt\":1' \"$TALLY_BRIEF\" || { echo 'phase one checkpoint has no prior steering' >&2; exit 1; }"
                ])
            );
            assert_eq!(failed_checkpoint["runtimeMaxSec"], 10);
            assert_eq!(failed_checkpoint["taskRef"], "fixture/phase-one-checkpoint");
            // The checkpoint lane prepares after this pass's own merges, so it
            // reads the tree task four just cleaned and is red on the steering
            // clause instead of the marker. Its verdict is still deferred by
            // the unrelated work this pass left outstanding, which is what the
            // deferral assertions above witness.
            assert!(task_capture(
                &daemon_paths,
                failed_checkpoint["anchor"].as_str().unwrap(),
                "phase-one-checkpoint"
            )
            .contains("phase one checkpoint has no prior steering"));

            assert_eq!(
                fixture_git(
                    &checkout,
                    &["show", &format!("{integration_branch}:build/four.txt")],
                ),
                "four"
            );
            assert_eq!(
                fixture_git(
                    &checkout,
                    &["show", &format!("{integration_branch}:build/six.txt")],
                ),
                "six"
            );
            assert!(
                StdCommand::new("git")
                    .arg("-C")
                    .arg(&checkout)
                    .args([
                        "cat-file",
                        "-e",
                        &format!("{integration_branch}:build/checkpoint-red"),
                    ])
                    .status()
                    .unwrap()
                    .code()
                    .is_some_and(|code| code != 0),
                "an independent task must be able to advance while the checkpoint is red"
            );

            // The unrelated cleanup task has merged, so the checkpoint's verdict
            // is now its own and a red run does spend an attempt.
            let checkpoint_steer = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_CHECKPOINT_STEER,
                &arguments("fixture-comment-10-checkpoint-steer", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let checkpoint_steer = runner_output(checkpoint_steer).await;
            assert!(
                checkpoint_steer.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&checkpoint_steer.stdout),
                String::from_utf8_lossy(&checkpoint_steer.stderr)
            );
            let steer_value = &flow_report(&checkpoint_steer)["report"]["finalValue"];
            assert_eq!(
                steer_value["reconciled"]["frontier"],
                json!(["phase-one-checkpoint"])
            );
            assert_eq!(
                steer_value["reconciled"]["deferrals"],
                json!([]),
                "no unrelated implementation work is left to defer the checkpoint"
            );
            assert_eq!(steer_value["state"], "steered");
            assert_eq!(steer_value["failures"][0]["taskId"], "phase-one-checkpoint");
            assert_eq!(steer_value["failures"][0]["stage"], "checkpoint");
            assert_eq!(
                steer_value["diagnoses"][0]["taskId"],
                "phase-one-checkpoint"
            );
            assert_eq!(steer_value["diagnoses"][0]["attempt"], 1);
            assert_eq!(steer_value["diagnoses"][0]["blocked"], false);
            assert_eq!(steer_value["checkpoints"], json!([]));
            assert_eq!(steer_value["continuation"]["created"], true);

            let checkpoint_pass = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_5,
                &arguments("fixture-comment-10-checkpoint", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let checkpoint_pass = runner_output(checkpoint_pass).await;
            assert!(
                checkpoint_pass.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&checkpoint_pass.stdout),
                String::from_utf8_lossy(&checkpoint_pass.stderr)
            );
            let checkpoint_value = &flow_report(&checkpoint_pass)["report"]["finalValue"];
            assert_eq!(
                checkpoint_value["reconciled"]["frontier"],
                json!(["phase-one-checkpoint"])
            );
            assert_eq!(checkpoint_value["state"], "advanced");
            assert_eq!(checkpoint_value["failures"], json!([]));
            assert_eq!(checkpoint_value["diagnoses"], json!([]));
            assert_eq!(checkpoint_value["merged"], json!([]));
            assert_eq!(checkpoint_value["continuation"]["created"], true);
            assert_eq!(
                checkpoint_value["reconciled"]["diagnoses"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|diagnosis| diagnosis["taskId"] == "phase-one-checkpoint")
                    .unwrap()["attempt"],
                1
            );
            assert_eq!(
                checkpoint_value["checkpoints"][0]["taskId"],
                "phase-one-checkpoint"
            );
            let checkpoint_revision = checkpoint_value["checkpoints"][0]["revision"]
                .as_str()
                .unwrap();
            assert_eq!(
                checkpoint_revision,
                fixture_git(&checkout, &["rev-parse", integration_branch])
            );
            let checkpoint_ref = checkpoint_value["checkpoints"][0]["ref"].as_str().unwrap();
            // Hidden namespace, never a tag: a public target repository must
            // not auto-fetch the campaign's checkpoint ledger.
            assert!(checkpoint_ref.starts_with("refs/tally/spec-build/v1/"));
            assert!(checkpoint_ref.contains("/checkpoint/phase-one-checkpoint-"));
            assert!(checkpoint_ref.ends_with(checkpoint_revision));
            assert_eq!(
                fixture_git(&checkout, &["ls-remote", "origin", checkpoint_ref])
                    .split_whitespace()
                    .next()
                    .unwrap(),
                checkpoint_revision
            );
            assert_eq!(fixture_git(&checkout, &["ls-remote", "--tags", "origin"]), "");

            // The gate proved this head, so the machine publishes it: `main`
            // fast-forwards to that exact revision and the receipt names it.
            // No operator act stands between the proof and the publication.
            let published = &checkpoint_value["published"];
            assert_eq!(published["action"], "fast-forward");
            assert_eq!(published["sha"], checkpoint_revision);
            assert_eq!(published["baseBranch"], "main");
            assert_eq!(published["receipt"]["sha"], checkpoint_revision);
            assert_eq!(
                published["receipt"]["provenBy"]["taskId"],
                "phase-one-checkpoint"
            );
            assert_eq!(published["receipt"]["provenBy"]["reference"], checkpoint_ref);
            assert_eq!(
                fixture_git(&checkout, &["ls-remote", "origin", "refs/heads/main"])
                    .split_whitespace()
                    .next()
                    .unwrap(),
                checkpoint_revision,
                "main advances by fast-forward of the gate-proven head"
            );
            let receipt_ref = published["receiptRef"].as_str().unwrap();
            assert!(receipt_ref.starts_with("refs/tally/spec-build/v1/"));
            assert!(receipt_ref.ends_with(checkpoint_revision));
            assert_eq!(
                fixture_git(&checkout, &["ls-remote", "origin", receipt_ref])
                    .split_whitespace()
                    .next()
                    .unwrap(),
                checkpoint_revision
            );
            assert_eq!(fixture_git(&checkout, &["ls-remote", "--tags", "origin"]), "");

            let checkpoint_items = wait_for_flow_items(&client, SPEC_BUILD_RUN_5, 8).await;
            assert_eq!(checkpoint_items.len(), 8);
            assert_eq!(
                checkpoint_items
                .iter()
                    .filter(|item| item["taskRef"] == "fixture/phase-one-checkpoint")
                    .count(),
                5,
                "the publication is the gate's own terminal act and carries its task ref"
            );
            assert_eq!(
                checkpoint_items
                    .iter()
                    .filter(|item| item.get("taskRef").is_none())
                    .count(),
                3
            );
            assert!(checkpoint_items.iter().all(|item| {
                item["orchestration"]["nodeLabel"] != "agent-phase-one-checkpoint"
                    && item["orchestration"]["nodeLabel"] != "publish-phase-one-checkpoint"
                    && item["orchestration"]["nodeLabel"] != "merge-phase-one-checkpoint"
            }));

            let task_2_base = fixture_git(&checkout, &["rev-parse", integration_branch]);

            let sixth = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_6,
                &arguments("fixture-comment-11", "high"),
                32,
            )
            .spawn()
            .unwrap();
            let sixth = runner_output(sixth).await;
            assert!(
                sixth.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&sixth.stdout),
                String::from_utf8_lossy(&sixth.stderr)
            );
            let sixth_value = &flow_report(&sixth)["report"]["finalValue"];
            assert_eq!(
                sixth_value["reconciled"]["frontier"],
                json!(["task-2"])
            );
            assert_eq!(
                sixth_value["reconciled"]["checkpoints"][0]["taskId"],
                "phase-one-checkpoint"
            );
            assert_eq!(sixth_value["state"], "steered");
            assert_eq!(sixth_value["merged"], json!([]));
            assert_eq!(sixth_value["failures"][0]["taskId"], "task-2");
            assert_eq!(sixth_value["failures"][0]["stage"], "gate:fixture-first");
            assert_eq!(sixth_value["diagnoses"][0]["taskId"], "task-2");
            assert_eq!(sixth_value["diagnoses"][0]["attempt"], 1);
            assert_eq!(sixth_value["diagnoses"][0]["blocked"], false);
            assert_eq!(sixth_value["continuation"]["created"], true);
            let sixth_submitted = runner_events(&sixth, "node-submitted");
            // #386: `tree-delta-task-2` -- task-2 passes ownership this pass,
            // so it reaches the gate. The fourteenth node is the standing
            // publication: a proven head is re-offered to `main` every pass,
            // which is what makes an interrupted fast-forward and a record
            // commit landing on `main` mid-campaign both self-repairing.
            assert_eq!(sixth_submitted.len(), 14);
            assert_eq!(
                sixth_value["published"]["action"], "already-published",
                "a pass that merged nothing re-offers the same proven head and moves nothing"
            );
            assert!(sixth_submitted
                .iter()
                .all(|event| event["disposition"] == "created"));
            assert_eq!(
                sixth_submitted
                    .iter()
                    .filter(|event| event["taskRef"] == "fixture/task-2")
                    .count(),
                10
            );
            assert_eq!(
                sixth_submitted
                    .iter()
                    .filter(|event| event.get("taskRef").is_none())
                    .count(),
                3,
                "a failure-only pass must sweep, reconcile, and post one continuation"
            );

            assert_eq!(
                fixture_git(
                    &checkout,
                    &["show", &format!("{integration_branch}:build/four.txt")],
                ),
                "four"
            );
            assert_eq!(
                fixture_git(
                    &checkout,
                    &["show", &format!("{integration_branch}:build/six.txt")],
                ),
                "six"
            );
            let integration_paths = fixture_git(
                &checkout,
                &["ls-tree", "-r", "--name-only", integration_branch],
            );
            assert!(!integration_paths.lines().any(|path| path == "build/two.txt"));
            assert!(!integration_paths.lines().any(|path| path == "build/five.txt"));
            assert_eq!(
                fixture_git(&checkout, &["rev-parse", integration_branch]),
                task_2_base,
                "the first failed task-2 attempt must leave integration unchanged"
            );
            assert!(
                !fixture_git(
                    &checkout,
                    &["ls-tree", "-r", "--name-only", integration_branch],
                )
                    .lines()
                    .any(|path| {
                        let basename = path.rsplit('/').next().unwrap_or(path);
                        basename.ends_with(".db")
                            || basename.ends_with(".db-wal")
                            || basename.ends_with(".db-shm")
                            || basename.contains(".sqlite")
                    })
            );
            let first_parent = fixture_git(
                &checkout,
                &[
                    "rev-list",
                    "--first-parent",
                    "--reverse",
                    integration_branch,
                ],
            );
            let commits = first_parent.lines().collect::<Vec<_>>();
            assert_eq!(commits.len(), 5, "initial commit plus four task merges");
            // One line of development. The shared remote base is not a second
            // line the campaign has to be bridged to: it is the gate-proven
            // prefix of this one, so it names the revision the gate proved and
            // holds nothing the integration line does not contain.
            fixture_git(&checkout, &["fetch", "origin"]);
            assert_eq!(
                fixture_git(&checkout, &["rev-parse", "origin/main"]),
                checkpoint_revision,
                "main names the gate-proven head it fast-forwarded to"
            );
            fixture_git(
                &checkout,
                &[
                    "merge-base",
                    "--is-ancestor",
                    "origin/main",
                    integration_branch,
                ],
            );

            let replay = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_4,
                &arguments("fixture-comment-10-frontier", "high"),
                48,
            )
            .spawn()
            .unwrap();
            let replay = runner_output(replay).await;
            assert_eq!(replay.status.code(), Some(1));
            let replay_failure = flow_failure(&replay);
            assert_eq!(replay_failure["error"]["name"], "SpecBuildReplayError");
            assert_eq!(replay_failure["error"]["code"], "campaign-replay-refused");
            assert_eq!(
                replay_failure["error"]["details"]["recovery"],
                "start a fresh reconcile pass"
            );
            let replayed = runner_events(&replay, "node-submitted");
            assert_eq!(replayed.len(), 1);
            assert_eq!(replayed[0]["label"], "spec-build-sweep");
            assert_eq!(replayed[0]["disposition"], "reused");
            assert_eq!(flow_items(&client, SPEC_BUILD_RUN_4).await.len(), 32);

            let seventh = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_7,
                &arguments("fixture-comment-12", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let seventh = runner_output(seventh).await;
            assert!(
                seventh.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&seventh.stdout),
                String::from_utf8_lossy(&seventh.stderr)
            );
            let seventh_value = &flow_report(&seventh)["report"]["finalValue"];
            assert_eq!(seventh_value["state"], "steered");
            assert_eq!(
                seventh_value["reconciled"]["frontier"],
                json!(["task-2"])
            );
            let prior_task_2 = seventh_value["reconciled"]["diagnoses"]
                .as_array()
                .unwrap()
                .iter()
                .find(|diagnosis| diagnosis["taskId"] == "task-2")
                .unwrap();
            assert_eq!(prior_task_2["attempt"], 1);
            assert_eq!(seventh_value["failures"][0]["taskId"], "task-2");
            assert_eq!(seventh_value["diagnoses"][0]["taskId"], "task-2");
            assert_eq!(seventh_value["diagnoses"][0]["attempt"], 2);
            assert_eq!(seventh_value["diagnoses"][0]["blocked"], true);
            assert_eq!(seventh_value["continuation"]["created"], true);
            let seventh_submitted = runner_events(&seventh, "node-submitted");
            // #386: `tree-delta-task-2` -- task-2 passes ownership this pass
            // too, so it reaches the gate again before blocking. The publish
            // node runs beside it and finds the same head already published.
            assert_eq!(seventh_submitted.len(), 14);
            assert_eq!(seventh_value["published"]["action"], "already-published");
            assert_eq!(
                seventh_submitted
                    .iter()
                    .filter(|event| event["taskRef"] == "fixture/task-2")
                    .count(),
                10
            );
            assert_eq!(
                seventh_submitted
                    .iter()
                    .filter(|event| event.get("taskRef").is_none())
                    .count(),
                3
            );

            let eighth = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_8,
                &arguments("fixture-comment-13", "low"),
                48,
            )
            .spawn()
            .unwrap();
            let eighth = runner_output(eighth).await;
            assert!(
                eighth.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&eighth.stdout),
                String::from_utf8_lossy(&eighth.stderr)
            );
            let eighth_value = &flow_report(&eighth)["report"]["finalValue"];
            assert_eq!(eighth_value["state"], "blocked");
            assert_eq!(
                eighth_value["reconciled"]["merged"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|fact| fact["taskId"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["task-1", "task-3", "task-4", "task-6"]
            );
            assert_eq!(
                eighth_value["reconciled"]["checkpoints"][0]["taskId"],
                "phase-one-checkpoint"
            );
            assert_eq!(
                eighth_value["reconciled"]["remaining"],
                json!(["task-2", "task-5"])
            );
            assert_eq!(eighth_value["reconciled"]["frontier"], json!([]));
            assert_eq!(eighth_value["reconciled"]["quiescent"], true);
            assert_eq!(
                eighth_value["reconciled"]["blocked"],
                json!([
                    {"taskId": "task-2", "blockedBy": ["task-2"]},
                    {"taskId": "task-5", "blockedBy": ["task-2"]}
                ])
            );
            assert_eq!(eighth_value["escalation"]["posted"], true);
            assert_eq!(eighth_value["escalation"]["diagnosisCount"], 4);
            let eighth_submitted = runner_events(&eighth, "node-submitted");
            // Sweep, reconcile, escalate -- and the publication, which a block
            // does not hold up: the head the gate proved is already on `main`,
            // and the blocked task is the operator's to answer, not its bar.
            assert_eq!(eighth_submitted.len(), 4);
            assert_eq!(
                eighth_submitted
                    .iter()
                    .filter(|event| event.get("taskRef").is_none())
                    .count(),
                3
            );
            assert_eq!(eighth_value["published"]["action"], "already-published");
            assert_eq!(
                eighth_value["published"]["sha"], checkpoint_revision,
                "a blocked campaign still names one published sha, and it is the proven one"
            );

            let attempt_receipts = fs::read_to_string(&attempt_receipts_path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(attempt_receipts.len(), 5);
            assert!(attempt_receipts.iter().enumerate().all(|(index, receipt)| {
                receipt["schemaVersion"].as_u64() == Some(2)
                    && receipt["sequence"].as_u64() == Some(index as u64 + 1)
                    && receipt["armSerial"].as_u64() == Some(1)
                    && receipt["worklistSha256"] == initial_worklist_sha256
                    && receipt["actor"] == "spec-build-driver"
                    && receipt["writtenAt"]
                        .as_str()
                        .is_some_and(|value| chrono::DateTime::parse_from_rfc3339(value).is_ok())
            }));
            let escalation = &attempt_receipts[4];
            assert_eq!(escalation["kind"], "escalation");
            let escalation_body = escalation["body"].as_str().unwrap();
            assert!(escalation_body.contains("Accumulated machine diagnoses"));
            assert!(escalation_body.contains("`task-1` attempt 1"));
            assert!(escalation_body.contains("`phase-one-checkpoint` attempt 1"));
            assert!(escalation_body.contains("`task-2` attempt 1"));
            assert!(escalation_body.contains("`task-2` attempt 2"));

            // Quiescence is a terminal outcome, so the escalation carries a
            // closing summary beside it: the same digest a completed campaign
            // renders, reflecting partial state.
            assert!(eighth_value["escalation"]["summary"]
                .as_str()
                .unwrap()
                .ends_with("/summary/quiescent"));
            let quiescent_summary_ref = fixture_git(
                &checkout,
                &[
                    "ls-remote",
                    "origin",
                    "refs/tally/spec-build/v1/*/summary/quiescent",
                ],
            );
            let quiescent_summary_oid = quiescent_summary_ref
                .split_whitespace()
                .next()
                .expect("local repository omitted the quiescent closing summary");
            let quiescent_summary: Value = serde_json::from_str(&fixture_git(
                &checkout,
                &["cat-file", "blob", quiescent_summary_oid],
            ))
            .unwrap();
            assert_eq!(quiescent_summary["kind"], "closing-summary");
            assert_eq!(quiescent_summary["outcome"], "quiescent");
            let quiescent_body = quiescent_summary["body"].as_str().unwrap();
            assert!(
                quiescent_body.contains("Campaign closed at frontier quiescence"),
                "{quiescent_body}"
            );
            // Partial state, from witnessed facts only: what merged, what a
            // checkpoint bound, what is blocked, and every steering note.
            assert!(quiescent_body.contains("5 of 7 task(s)"), "{quiescent_body}");
            for fragment in [
                "#### Merged",
                "#### Checkpoints passed",
                "#### Blocked",
                "#### Steering notes issued",
                "`task-1`",
                "`task-6`",
                "`phase-one-checkpoint`",
                "`task-2` attempt 2",
            ] {
                assert!(
                    quiescent_body.contains(fragment),
                    "closing summary is missing {fragment}: {quiescent_body}"
                );
            }
            let task_1_diagnosis = attempt_receipts
                .iter()
                .find(|receipt| {
                    receipt["kind"] == "diagnosis"
                        && receipt["taskId"] == "task-1"
                        && receipt["attempt"] == 1
                })
                .expect("attempt log omitted task 1 diagnosis");
            assert!(task_1_diagnosis["diagnosis"]
                .as_str()
                .unwrap()
                .contains("[redacted-token]"));
            assert!(!task_1_diagnosis["diagnosis"]
                .as_str()
                .unwrap()
                .contains("ghp_"));
            for pattern in [
                "refs/tally/spec-build/v1/*/diagnosis/*",
                "refs/tally/spec-build/v1/*/retry/*",
                "refs/tally/spec-build/v1/*/escalation",
            ] {
                assert_eq!(
                    fixture_git(&checkout, &["ls-remote", "origin", pattern]),
                    "",
                    "attempt counter leaked into the ref namespace: {pattern}"
                );
            }

            let ninth = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_9,
                &arguments("fixture-comment-14", "low"),
                48,
            )
            .spawn()
            .unwrap();
            let ninth = runner_output(ninth).await;
            assert!(
                ninth.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&ninth.stdout),
                String::from_utf8_lossy(&ninth.stderr)
            );
            let ninth_value = &flow_report(&ninth)["report"]["finalValue"];
            assert_eq!(ninth_value["state"], "blocked");
            assert!(ninth_value["reconciled"]["escalation"]
                .as_str()
                .unwrap()
                .ends_with("/attempt-receipts/5"));
            assert_eq!(ninth_value["escalation"], Value::Null);
            let ninth_submitted = runner_events(&ninth, "node-submitted");
            assert_eq!(
                ninth_submitted.len(),
                3,
                "escalation must be projected once, beside the sweep and the publication"
            );
            assert!(!ninth_submitted
                .iter()
                .any(|event| event["label"] == "spec-build-escalate"));
            assert_eq!(
                ninth_submitted
                    .iter()
                    .filter(|event| event.get("taskRef").is_none())
                    .count(),
                2
            );
            assert_eq!(ninth_value["published"]["action"], "already-published");
            assert!(
                !fixture_git(&checkout, &["worktree", "list", "--porcelain"])
                    .contains(workspace_root.to_str().unwrap())
            );

            pause(&client, "campaign-control").await;
            let attached_arguments = arguments("attached-comment", "low");
            let attached_first = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_ATTACHED_RUN,
                &attached_arguments,
                32,
            )
            .spawn()
            .unwrap();
            let attached_items =
                wait_for_flow_state(&client, SPEC_BUILD_ATTACHED_RUN, 1, "paused").await;
            assert_eq!(
                attached_items[0]["orchestration"]["nodeLabel"],
                "spec-build-sweep"
            );
            let attached_stdout = temp.path().join("attached-replay.out");
            let mut attached_second_command = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_ATTACHED_RUN,
                &attached_arguments,
                32,
            );
            attached_second_command
                .stdout(Stdio::from(fs::File::create(&attached_stdout).unwrap()));
            let attached_second = attached_second_command.spawn().unwrap();
            let (attached_deadline, attached_budget) = poll_deadline();
            loop {
                if fs::read_to_string(&attached_stdout)
                    .unwrap_or_default()
                    .contains("\"disposition\":\"attached\"")
                    || tokio::time::Instant::now() >= attached_deadline
                {
                    break;
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            let attached_log = fs::read_to_string(&attached_stdout).unwrap();
            assert!(
                attached_log.contains("\"disposition\":\"attached\""),
                "the concurrent replay did not attach to the live sweep {attached_budget}: \
                 {attached_log}"
            );
            resume_all(&client).await;
            let (attached_first, attached_second) = tokio::join!(
                runner_output(attached_first),
                runner_output(attached_second)
            );
            for output in [&attached_first, &attached_second] {
                assert!(
                    output.status.success(),
                    "attached campaign runner failed:\n{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            assert_eq!(
                wait_for_flow_items(&client, SPEC_BUILD_ATTACHED_RUN, 3)
                    .await
                    .len(),
                3,
                "an attached live replay must share sweep, reconcile, and publish nodes"
            );

            let agent_order = fs::read_to_string(control.join("agent-order.log")).unwrap();
            let agents = agent_order.lines().collect::<Vec<_>>();
            assert_eq!(agents.len(), 7);
            let mut first_frontier = agents[..2].to_vec();
            first_frontier.sort_unstable();
            assert_eq!(first_frontier, vec!["task-1", "task-3"]);
            assert_eq!(agents[2], "task-1");
            let mut fourth_frontier = agents[3..5].to_vec();
            fourth_frontier.sort_unstable();
            assert_eq!(fourth_frontier, vec!["task-4", "task-6"]);
            assert_eq!(agents[5], "task-2");
            assert_eq!(agents[6], "task-2");

            let policy_order = fs::read_to_string(control.join("policy-order.log")).unwrap();
            let policies = policy_order.lines().collect::<Vec<_>>();
            assert_eq!(policies.len(), 11);
            assert_eq!(
                policies
                    .iter()
                    .filter(|line| line.starts_with("diagnosis:"))
                    .count(),
                4
            );
            assert_eq!(
                policies
                    .iter()
                    .filter(|line| line.starts_with("implementation:"))
                    .count(),
                7
            );
            assert!(policies.iter().all(|line| {
                if line.starts_with("diagnosis:") {
                    line.ends_with(":approval_policy=\"never\":read-only")
                } else {
                    line.ends_with(":approval_policy=\"never\":danger-full-access")
                }
            }));

            let diagnosis_inputs =
                fs::read_to_string(control.join("diagnosis-inputs.log")).unwrap();
            let diagnosis_inputs = diagnosis_inputs
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(diagnosis_inputs.len(), 4);
            assert_eq!(diagnosis_inputs[0]["task"], "task-1");
            assert_eq!(diagnosis_inputs[0]["previous"], 0);
            assert_eq!(diagnosis_inputs[0]["hasPatch"], true);
            assert_eq!(
                diagnosis_inputs[0]["hasForbidPathsHistoryRule"],
                true,
                "the forbidPaths steward context omitted its history-walk cure"
            );
            assert_eq!(diagnosis_inputs[1]["task"], "phase-one-checkpoint");
            assert_eq!(diagnosis_inputs[1]["previous"], 0);
            assert_eq!(diagnosis_inputs[1]["hasPatch"], false);
            assert_eq!(diagnosis_inputs[2]["task"], "task-2");
            assert_eq!(diagnosis_inputs[2]["previous"], 0);
            assert_eq!(diagnosis_inputs[2]["hasPatch"], true);
            assert_eq!(diagnosis_inputs[3]["task"], "task-2");
            assert_eq!(diagnosis_inputs[3]["previous"], 1);
            assert_eq!(diagnosis_inputs[3]["hasPatch"], true);
            assert!(diagnosis_inputs.iter().all(|receipt| {
                receipt["hasBrief"] == true
                    && receipt["hasDiff"] == true
                    && receipt["hasStderr"] == true
                    && receipt["gateCount"].as_u64().unwrap() >= 1
            }));

            let gated = fs::read_to_string(control.join("gate-order.log")).unwrap();
            let gated = gated.lines().collect::<Vec<_>>();
            for (receipt, expected) in [
                ("task-1:first", 1),
                ("task-1:second", 1),
                ("task-2:first", 2),
                ("task-2:second", 0),
                ("task-3:first", 1),
                ("task-3:second", 1),
                ("task-4:first", 1),
                ("task-4:second", 1),
                ("task-6:first", 2),
                ("task-6:second", 2),
            ] {
                assert_eq!(
                    gated
                        .iter()
                        .filter(|candidate| **candidate == receipt)
                        .count(),
                    expected,
                    "unexpected gate count for {receipt}: {gated:?}"
                );
            }

            // campaigns.md invites worklist edits between passes. Renaming a task
            // that carries two diagnosis receipts must degrade the campaign's
            // memory of those attempts, not brick reconciliation.
            fixture_git(&checkout, &["fetch", "origin"]);
            fixture_git(&checkout, &["reset", "--hard", "origin/main"]);
            let worklist_path = checkout.join("specs/001-toy/tasks.json");
            let renamed_worklist = fs::read_to_string(&worklist_path)
                .unwrap()
                .replace("\"task-2\"", "\"task-2b\"");
            fs::write(&worklist_path, renamed_worklist).unwrap();
            fixture_git(&checkout, &["add", "specs/001-toy/tasks.json"]);
            fixture_git(&checkout, &["commit", "-m", "operator: rename the diagnosed task"]);
            fixture_git(&checkout, &["push", "origin", "main"]);
            let renamed_worklist_sha256 = write_receipt_authority(2);

            let renamed = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_RENAMED,
                &arguments("fixture-comment-15-renamed", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let renamed = runner_output(renamed).await;
            assert!(
                renamed.status.success(),
                "a worklist rename must not brick the campaign\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&renamed.stdout),
                String::from_utf8_lossy(&renamed.stderr)
            );
            let renamed_value = &flow_report(&renamed)["report"]["finalValue"];
            let renamed_warnings = renamed_value["reconciled"]["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|warning| warning.as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                renamed_warnings
                    .iter()
                    .filter(|warning| warning.contains("'task-2'")
                        && warning.contains("no longer names that task"))
                    .count(),
                2,
                "both orphaned task-2 receipts must be witnessed: {renamed_warnings:?}"
            );
            assert_eq!(
                renamed_value["reconciled"]["blocked"],
                json!([]),
                "receipts for a dropped task must stop blocking the renamed subtree"
            );
            assert_eq!(renamed_value["reconciled"]["quiescent"], false);
            assert!(renamed_value["reconciled"]["remaining"]
                .as_array()
                .unwrap()
                .iter()
                .any(|task| task == "task-2b"));
            assert_eq!(renamed_value["state"], "advanced");
            assert_eq!(
                renamed_value["checkpoints"][0]["taskId"],
                "phase-one-checkpoint",
                "the checkpoint rebinds to the edited worklist digest"
            );
            assert_eq!(
                renamed_value["reconciled"]["escalation"],
                Value::Null,
                "the dropped task's escalation must not survive into the renamed input epoch"
            );

            // Campaign machinery, not the task's work: an unwritable workspace
            // root denies the merge node the integration checkout it stages
            // there. That fault must buy a retry rather than spend one of the
            // renamed task's two steering attempts.
            let sealed = fs::metadata(&workspace_root).unwrap().permissions();
            let mut locked = sealed.clone();
            locked.set_mode(0o500);
            fs::set_permissions(&workspace_root, locked).unwrap();
            let faulted = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_MACHINERY,
                &arguments("fixture-comment-16-machinery", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let faulted = runner_output(faulted).await;
            fs::set_permissions(&workspace_root, sealed).unwrap();
            assert!(
                faulted.status.success(),
                "a machinery fault must settle into a retry\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&faulted.stdout),
                String::from_utf8_lossy(&faulted.stderr)
            );
            let faulted_value = &flow_report(&faulted)["report"]["finalValue"];
            assert_eq!(faulted_value["reconciled"]["frontier"], json!(["task-2b"]));
            assert_eq!(faulted_value["failures"][0]["taskId"], "task-2b");
            assert_eq!(
                faulted_value["failures"][0]["stage"],
                "merge",
                "faulted report: {}",
                serde_json::to_string_pretty(faulted_value).unwrap()
            );
            assert_eq!(faulted_value["state"], "retrying");
            assert_eq!(
                faulted_value["diagnoses"],
                json!([]),
                "campaign machinery says nothing about whether the work is wrong"
            );
            assert_eq!(faulted_value["retries"][0]["taskId"], "task-2b");
            assert_eq!(faulted_value["retries"][0]["attempt"], 1);
            assert_eq!(faulted_value["retries"][0]["posted"], true);
            assert_eq!(faulted_value["retries"][0]["exhausted"], false);
            assert_eq!(
                faulted_value["continuation"]["created"],
                true,
                "the pass must write the event that resumes the retry"
            );
            let attempt_receipts = fs::read_to_string(&attempt_receipts_path)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            let retry = attempt_receipts
                .iter()
                .find(|receipt| {
                    receipt["kind"] == "retry"
                        && receipt["taskId"] == "task-2b"
                        && receipt["attempt"] == 1
                })
                .expect("attempt log omitted the machinery retry receipt");
            assert_eq!(retry["sequence"], 6);
            assert_eq!(retry["kind"], "retry");
            assert_eq!(retry["taskId"], "task-2b");
            assert_eq!(retry["schemaVersion"], 2);
            assert_eq!(retry["armSerial"], 2);
            assert_eq!(retry["worklistSha256"], renamed_worklist_sha256);
            assert_eq!(retry["actor"], "spec-build-driver");
            assert!(chrono::DateTime::parse_from_rfc3339(
                retry["writtenAt"].as_str().unwrap()
            )
            .is_ok());
            assert!(retry["reason"].as_str().unwrap().contains("`merge`"));

            let recovered = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_RECOVERED,
                &arguments("fixture-comment-17-recovered", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let recovered = runner_output(recovered).await;
            assert!(
                recovered.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&recovered.stdout),
                String::from_utf8_lossy(&recovered.stderr)
            );
            let recovered_value = &flow_report(&recovered)["report"]["finalValue"];
            assert_eq!(
                recovered_value["reconciled"]["retries"][0]["taskId"],
                "task-2b",
                "the retry receipt is durable local state"
            );
            assert_eq!(
                recovered_value["reconciled"]["diagnoses"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|diagnosis| diagnosis["taskId"] == "task-2b")
                    .count(),
                0,
                "the machinery fault spent no steering attempt"
            );
            assert_eq!(recovered_value["state"], "advanced");
            assert_eq!(recovered_value["merged"][0]["taskId"], "task-2b");

            assert_eq!(
                fixture_git(
                    &checkout,
                    &["show", &format!("{integration_branch}:build/two.txt")],
                ),
                "two"
            );

            // task-5 is beyond what the fixture agent implements, so both its
            // implementation and its diagnosis die. A steering lane that throws
            // must still leave the campaign a durable continuation to resume from.
            let halted = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_HALTED,
                &arguments("fixture-comment-18-halted", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let halted = runner_output(halted).await;
            assert_eq!(halted.status.code(), Some(1));
            let halted_submitted = runner_events(&halted, "node-submitted");
            assert!(
                halted_submitted
                    .iter()
                    .any(|event| event["label"] == "diagnose-task-5"),
                "the diagnosis lane must have been dispatched"
            );
            assert!(
                halted_submitted
                    .iter()
                    .any(|event| event["label"] == "spec-build-continue"),
                "a thrown steering lane must not swallow the continuation: {:?}",
                halted_submitted
                    .iter()
                    .map(|event| event["label"].as_str().unwrap_or("<missing>"))
                    .collect::<Vec<_>>()
            );
            let continuation_ref = fixture_git(
                &checkout,
                &[
                    "ls-remote",
                    "origin",
                    "refs/tally/spec-build/v1/*/continuation/*",
                ],
            );
            assert!(
                !continuation_ref.trim().is_empty(),
                "the campaign must carry a durable continuation receipt"
            );

            // The operator drops the task no agent in this fixture can build.
            // That is an ordinary worklist edit, and it leaves the campaign one
            // checkpoint short of done -- so the next two passes walk the other
            // terminal outcome: completion, and the closing summary it renders.
            fixture_git(&checkout, &["fetch", "origin"]);
            fixture_git(&checkout, &["reset", "--hard", "origin/main"]);
            let dropped_worklist: Value =
                serde_json::from_str(&fs::read_to_string(&worklist_path).unwrap()).unwrap();
            let dropped_tasks = dropped_worklist["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|task| task["id"] != "task-5")
                .cloned()
                .collect::<Vec<_>>();
            fs::write(
                &worklist_path,
                serde_json::to_string_pretty(&json!({
                    "schemaVersion": dropped_worklist["schemaVersion"],
                    "tasks": dropped_tasks,
                }))
                .unwrap(),
            )
            .unwrap();
            fixture_git(&checkout, &["add", "specs/001-toy/tasks.json"]);
            fixture_git(&checkout, &["commit", "-m", "operator: drop the unbuildable task"]);
            fixture_git(&checkout, &["push", "origin", "main"]);
            write_receipt_authority(3);

            // Editing the worklist rotates its digest, so the checkpoint must
            // rebind to the edited worklist before the campaign can be done.
            let last_task = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_LAST_TASK,
                &arguments("fixture-comment-19-last-task", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let last_task = runner_output(last_task).await;
            assert!(
                last_task.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&last_task.stdout),
                String::from_utf8_lossy(&last_task.stderr)
            );
            let last_task_value = &flow_report(&last_task)["report"]["finalValue"];
            assert_eq!(last_task_value["state"], "advanced");
            assert_eq!(
                last_task_value["checkpoints"][0]["taskId"],
                "phase-one-checkpoint"
            );
            assert_eq!(
                last_task_value["reconciled"]["closingSummary"],
                Value::Null,
                "a pass that still has work is not a terminal pass"
            );
            // The operator's two worklist edits landed on `main`, so the two
            // lines diverged over a path no lane touched. This is the whole
            // wedge the single-line model deletes: no commit is copied across
            // by hand and nothing is published on the strength of an old
            // proof. The machinery rebases the integration line onto `main`
            // and leaves `main` exactly where it was until a gate has seen the
            // rebased head.
            assert_eq!(last_task_value["published"]["action"], "rebase-and-regate");
            assert_eq!(last_task_value["published"]["regateRequired"], true);
            assert_eq!(last_task_value["published"]["sha"], Value::Null);
            assert_eq!(last_task_value["published"]["receipt"], Value::Null);
            let rebased_head = last_task_value["published"]["integrationHead"]
                .as_str()
                .unwrap()
                .to_owned();
            fixture_git(&checkout, &["fetch", "origin"]);
            assert_eq!(
                fixture_git(&checkout, &["rev-parse", integration_branch]),
                rebased_head
            );
            fixture_git(
                &checkout,
                &["merge-base", "--is-ancestor", "origin/main", &rebased_head],
            );
            assert_ne!(
                fixture_git(&checkout, &["rev-parse", "origin/main"]),
                rebased_head,
                "an unproven head never moves main"
            );

            // The re-gate is an ordinary pass: the checkpoint's own ref names
            // the revision it proved, so a rebased head is simply not proven
            // yet and the gate runs again. Only then does main advance.
            let regated = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_REGATE,
                &arguments("fixture-comment-19-regate", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let regated = runner_output(regated).await;
            assert!(
                regated.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&regated.stdout),
                String::from_utf8_lossy(&regated.stderr)
            );
            let regated_value = &flow_report(&regated)["report"]["finalValue"];
            assert_eq!(regated_value["state"], "advanced");
            assert_eq!(
                regated_value["checkpoints"][0]["taskId"],
                "phase-one-checkpoint"
            );
            assert_eq!(regated_value["checkpoints"][0]["revision"], rebased_head);
            assert_eq!(regated_value["published"]["action"], "fast-forward");
            assert_eq!(regated_value["published"]["sha"], rebased_head);
            fixture_git(&checkout, &["fetch", "origin"]);
            assert_eq!(
                fixture_git(&checkout, &["rev-parse", "origin/main"]),
                rebased_head,
                "a published sha is always a gated sha"
            );

            let completed = runner(
                &config_path,
                &daemon_paths.socket,
                &script,
                SPEC_BUILD_RUN_COMPLETE,
                &arguments("fixture-comment-20-complete", "low"),
                32,
            )
            .spawn()
            .unwrap();
            let completed = runner_output(completed).await;
            assert!(
                completed.status.success(),
                "stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&completed.stdout),
                String::from_utf8_lossy(&completed.stderr)
            );
            let completed_value = &flow_report(&completed)["report"]["finalValue"];
            assert_eq!(completed_value["state"], "complete");
            assert_eq!(completed_value["reconciled"]["remaining"], json!([]));
            assert!(completed_value["reconciled"]["closingSummary"]
                .as_str()
                .unwrap()
                .ends_with("/summary/complete"));

            let complete_summary_ref = fixture_git(
                &checkout,
                &[
                    "ls-remote",
                    "origin",
                    "refs/tally/spec-build/v1/*/summary/complete",
                ],
            );
            let complete_summary_oid = complete_summary_ref
                .split_whitespace()
                .next()
                .expect("local repository omitted the completion closing summary");
            let complete_summary: Value = serde_json::from_str(&fixture_git(
                &checkout,
                &["cat-file", "blob", complete_summary_oid],
            ))
            .unwrap();
            assert_eq!(complete_summary["kind"], "closing-summary");
            assert_eq!(complete_summary["outcome"], "complete");
            let complete_body = complete_summary["body"].as_str().unwrap();
            assert!(
                complete_body.contains("tally:campaign-complete:v1 source=sha256:"),
                "{complete_body}"
            );
            assert!(complete_body.contains("### Campaign complete"), "{complete_body}");
            assert!(complete_body.contains("6 of 6 task(s)"), "{complete_body}");
            for fragment in ["`task-1`", "`task-2b`", "`task-6`", "`phase-one-checkpoint`"] {
                assert!(
                    complete_body.contains(fragment),
                    "closing summary is missing {fragment}: {complete_body}"
                );
            }
            assert!(
                !complete_body.contains("#### Blocked"),
                "a completed campaign has nothing blocked: {complete_body}"
            );

            daemon.stop().await;
        })
        .await;
}

/// The events-dir continuation is the campaign's whole self-re-entry path, so
/// this proves the local loop end to end: the packaged driver writes
/// one bounded payload, the daemon's drain admits it, and a second identical
/// drop -- the shape a `tally-campaign-poll.timer` race produces -- resolves to
/// an attach against the live job instead of a second pass.
#[tokio::test(flavor = "current_thread")]
async fn spec_build_continuation_event_admits_one_pass_and_attaches_the_duplicate() {
    let _environment = ENVIRONMENT_LOCK.lock().await;
    tokio::task::LocalSet::new()
        .run_until(async {
            let temp = tempfile::tempdir().unwrap();
            let checkout = temp.path().join("checkout");
            let remote = temp.path().join("remote.git");
            fs::create_dir_all(&checkout).unwrap();
            fixture_git(
                temp.path(),
                &["init", "--bare", "--initial-branch=main", "remote.git"],
            );
            fixture_git(&checkout, &["init", "--initial-branch=main"]);
            fixture_git(
                &checkout,
                &["remote", "add", "origin", remote.to_str().unwrap()],
            );

            let mut config = config();
            config.pools.insert(
                "fixture-campaign".to_owned(),
                PoolConfig {
                    resource: Some(ResourceKind::Mutex),
                    capacity: 1,
                    predicate: PoolPredicate::CoResidency(CoResidencyPredicate {}),
                    ..PoolConfig::default()
                },
            );
            config.validate().unwrap();

            let daemon_paths = paths(&temp.path().join("daemon"));
            let daemon = start_daemon(&daemon_paths, config).await;
            let client = rpc(&daemon_paths.socket).await;
            // Nothing may execute: a queued pass is what the duplicate has to
            // attach to, and the capacity-1 mutex is what holds it there.
            pause(&client, "fixture-campaign").await;

            let events_dir = daemon_paths.events_dir();
            let brief_path = temp.path().join("continue-brief.json");
            fs::write(
                &brief_path,
                serde_json::to_vec(&json!({
                    "campaign": "fixture",
                    "repository": "acme/spec",
                    "repositoryConfig": {
                        "checkout": checkout,
                        "baseBranch": "main",
                        "remote": "origin",
                        "forge": "local"
                    },
                    "issue": {
                        "number": "7",
                        "url": "local://acme/spec/issues/7"
                    },
                    "runId": "pass-1",
                    "continuation": {
                        "argv": ["/bin/true"],
                        "pool": ["flow", "fixture-campaign"],
                        "priority": "low",
                        "runtimeMaxSec": 60,
                        "eventsDir": events_dir
                    },
                    "brief": null
                }))
                .unwrap(),
            )
            .unwrap();

            let run_continue = || {
                let output = StdCommand::new(rust_spec_build_driver())
                    .arg("continue")
                    .env("TALLY_BRIEF", &brief_path)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "driver continue failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let stdout = String::from_utf8(output.stdout).unwrap();
                let line = stdout
                    .lines()
                    .find_map(|line| line.strip_prefix("TALLY_FINAL_MESSAGE="))
                    .expect("driver emitted no final message");
                serde_json::from_str::<Value>(line).unwrap()
            };

            let first = run_continue();
            assert_eq!(first["created"], true);
            assert!(first["receipt"]
                .as_str()
                .unwrap()
                .contains("/continuation/"));
            let event_path = PathBuf::from(first["event"].as_str().unwrap());
            assert_eq!(event_path.parent().unwrap(), events_dir);
            let payload: Value = serde_json::from_slice(&fs::read(&event_path).unwrap()).unwrap();
            assert_eq!(payload["dedupKey"], first["dedupKey"]);

            let drained = client.call("queue.drain", None).await.unwrap();
            assert_eq!(drained["enqueued"], 1, "{drained}");
            assert_eq!(drained["rejected"], 0, "{drained}");
            assert!(!event_path.exists(), "a drained event must be archived");

            let admitted = flow_jobs_with_dedup(&client, first["dedupKey"].as_str().unwrap()).await;
            assert_eq!(admitted.len(), 1, "{admitted:?}");
            let task_uuid = admitted[0]["taskUuid"].as_str().unwrap().to_owned();
            assert_eq!(admitted[0]["source"], "events-dir");
            assert_eq!(admitted[0]["disposition"], "created");
            assert_eq!(admitted[0]["argv"], json!(["/bin/true"]));
            assert_eq!(admitted[0]["pool"], json!(["fixture-campaign", "flow"]));
            // Held by the capacity-1 campaign mutex, so the duplicate below has
            // a live job to attach to instead of starting a second pass.
            assert_eq!(admitted[0]["rowStatus"], "pending");
            assert_eq!(admitted[0]["liveState"], "paused");

            // The same pass, replayed: byte-identical payload, same identity.
            let second = run_continue();
            assert_eq!(second["dedupKey"], first["dedupKey"]);
            assert_eq!(second["event"], first["event"]);
            assert_eq!(second["created"], true);
            let replayed: Value = serde_json::from_slice(&fs::read(&event_path).unwrap()).unwrap();
            assert_eq!(replayed, payload);

            // The poll-timer race: the identical payload straight through the
            // enqueue kernel resolves against the live job.
            let raced = client
                .call("queue.enqueue", Some(payload.clone()))
                .await
                .unwrap();
            assert_eq!(raced["disposition"], "attached", "{raced}");
            assert_eq!(raced["task_uuid"], task_uuid);

            let redrained = client.call("queue.drain", None).await.unwrap();
            assert_eq!(redrained["enqueued"], 1, "{redrained}");
            assert_eq!(redrained["rejected"], 0, "{redrained}");

            let after = flow_jobs_with_dedup(&client, first["dedupKey"].as_str().unwrap()).await;
            assert_eq!(
                after.len(),
                1,
                "a duplicate event and a timer race must yield exactly one pass: {after:?}"
            );
            assert_eq!(after[0]["taskUuid"], task_uuid);
            assert_eq!(after[0]["rowStatus"], "pending");

            daemon.stop().await;
        })
        .await;
}

async fn flow_jobs_with_dedup(client: &RpcClient, dedup_key: &str) -> Vec<Value> {
    let page = client
        .call("query.jobs", Some(json!({"limit": 1000})))
        .await
        .unwrap();
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["dedupKey"] == dedup_key)
        .cloned()
        .collect()
}
