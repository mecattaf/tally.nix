use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tally_client::RpcClient;
use tally_core::adapters::AdapterConfig;
use tally_core::config::{
    CoResidencyPredicate, Config, JournaldConfig, PoolConfig, PoolPredicate, ResourceKind,
};
use tally_core::daemon::{Daemon, DaemonError, DaemonPaths, DaemonSettings};
use tally_core::evidence::RetryPolicy;
use tally_core::executor::{
    read_exit_record, ExecutionPaths, Executor, ExecutorError, LocalUnitFact, LocalUnitProbe,
    LocalUnitState, UnitLimits,
};
use tally_core::producers::{EmitOutcome, GhObservation, ProducerConfig, ProducerEngine};
use tally_core::recovery::RecoveryPolicy;
use tally_core::taskdb::{
    read_acknowledged_events, related_trigger_from_gh_origin, EnqueueSource, GhContextSnapshot,
    GhItemState, GhItemType, GhTriggeringComment, GH_CONTEXT_SCHEMA_VERSION,
};
use tally_core::wire::EnqueuePayload;
use tally_core::witness::read_verified_records;
use tokio::process::{Child, Command};
use tokio::sync::{watch, Mutex};
use tokio::task::JoinHandle;

const CONCURRENT_RUN: &str = "00000000-0000-4000-8000-000000000501";
const KILLED_RUN: &str = "00000000-0000-4000-8000-000000000502";
const RESTARTED_RUN: &str = "00000000-0000-4000-8000-000000000503";
const DIVERGENT_RUN: &str = "00000000-0000-4000-8000-000000000504";
const DRV_BUILD_RUN: &str = "00000000-0000-4000-8000-000000000505";
const DRV_SUBSTITUTE_RUN: &str = "00000000-0000-4000-8000-000000000506";
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
        tokio::time::timeout(Duration::from_secs(10), self.task)
            .await
            .expect("daemon shutdown timed out")
            .expect("daemon task panicked")
            .expect("daemon shutdown failed");
    }
}

fn config() -> Config {
    let pool = |resource| PoolConfig {
        resource,
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

fn install_github_flow_producer(config: &mut Config, argv: Vec<String>) {
    let producer: ProducerConfig = serde_json::from_value(json!({
        "kind": "gh",
        "enable": true,
        "sources": [{"search": {"repo": "acme/widgets"}}],
        "triggers": {"commandComments": ["/pooled-review"]},
        "allowedActors": ["maintainer"],
        "postReceipt": false,
        "postEvidence": true,
        "closeOnPass": false,
        "neverMutate": false,
        "enqueue": {
            "argv": argv,
            "pool": "flow",
            "evidence": ["exit:0"],
            "noEnqueue": false
        }
    }))
    .unwrap();
    config.producers.insert("github-flow".to_owned(), producer);
}

fn install_fake_gh(root: &Path) -> (std::path::PathBuf, PathGuard) {
    let bin = root.join("fake-bin");
    fs::create_dir_all(&bin).unwrap();
    let requests = root.join("gh-requests.jsonl");
    let gh = bin.join("gh");
    fs::write(
        &gh,
        format!(
            concat!(
                "#!/bin/sh\n",
                "[ \"$1 $2 $3 $4\" = 'api graphql --input -' ] || exit 91\n",
                "request=$(cat)\n",
                "printf '%s\\n' \"$request\" >> '{}'\n",
                "case \"$request\" in\n",
                "  *TallyCompletionState*) printf '{{\"data\":{{\"node\":{{\"__typename\":\"Issue\",\"state\":\"OPEN\",\"comments\":{{\"nodes\":[],\"pageInfo\":{{\"hasNextPage\":false,\"endCursor\":null}}}}}}}}}}' ;;\n",
                "  *TallyCompletionComment*) printf '{{\"data\":{{\"addComment\":{{}}}}}}' ;;\n",
                "  *) exit 92 ;;\n",
                "esac\n"
            ),
            requests.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    let path_guard = PathGuard::prepend(&bin);
    (requests, path_guard)
}

fn install_fake_nix(root: &Path) -> (std::path::PathBuf, std::path::PathBuf, PathGuard) {
    let bin = root.join("fake-nix-bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = root.join("store-output-valid");
    let builds = root.join("nix-builds");
    let nix = bin.join("nix");
    fs::write(
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
    )
    .unwrap();
    let nix_store = bin.join("nix-store");
    fs::write(
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
    )
    .unwrap();
    for program in [&nix, &nix_store] {
        fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path_guard = PathGuard::prepend(&bin);
    (marker, builds, path_guard)
}

fn github_flow_observation() -> GhObservation {
    GhObservation {
        source: "search".to_owned(),
        repo: "acme/widgets".to_owned(),
        number: 61,
        html_url: "https://github.com/acme/widgets/issues/61".to_owned(),
        item_type: GhItemType::Issue,
        head_sha: None,
        node_id: "I_flow_61".to_owned(),
        item_author: "flow-author".to_owned(),
        trigger_actor: "maintainer".to_owned(),
        self_actor: "tally-bot".to_owned(),
        notification_reason: None,
        trigger_kind: "command-comment".to_owned(),
        event_id: Some("notification-61".to_owned()),
        comment_id: Some("comment-61".to_owned()),
        trigger_timestamp: "2026-07-26T12:30:00Z".to_owned(),
        trigger_value: None,
        context: GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: "Run pooled review".to_owned(),
            body: "Untrusted issue body never becomes flow code".to_owned(),
            state: Some(GhItemState::Open),
            head_sha: None,
            labels: vec!["ready".to_owned()],
            assignees: vec!["tally-bot".to_owned()],
            triggering_comment: Some(GhTriggeringComment {
                id: "comment-61".to_owned(),
                author: "maintainer".to_owned(),
                body: "/pooled-review".to_owned(),
            }),
        },
    }
}

fn settings() -> DaemonSettings {
    DaemonSettings {
        unit_limits: UnitLimits {
            cpu_weight: 100,
            memory_max_bytes: 64 * 1024 * 1024,
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
    let executor = Executor::new(&paths.state_dir, env!("CARGO_BIN_EXE_tally"))
        .with_systemd_run(paths.state_dir.join("absent-systemd-run"))
        .with_unit_probe(ExitFileProbe);
    let daemon = Daemon::open_with_executor(config, paths.clone(), settings(), executor)
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

fn one_node_source() -> &'static str {
    r#"
export const meta = {
  name: "github-flow",
  description: "GitHub provenance integration",
  pools: ["alpha"],
  argsSchema: { type: "object", additionalProperties: false },
  selectors: [],
  maxNodes: 1
};

(async () => sh(["/bin/sh", "-c", "exit 0"], {
  pools: ["alpha"], evidence: ["exit:0"], label: "github-child"
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
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
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
    client
        .call(
            "query.jobs",
            Some(json!({"flowRun": flow_run_id, "limit": 1000})),
        )
        .await
        .unwrap()["items"]
        .as_array()
        .unwrap()
        .clone()
}

async fn wait_for_flow_items(client: &RpcClient, flow_run_id: &str, expected: usize) -> Vec<Value> {
    for _ in 0..400 {
        let items = flow_items(client, flow_run_id).await;
        if items.len() == expected {
            return items;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("flow run {flow_run_id} did not reach {expected} durable rows");
}

async fn await_items(client: &RpcClient, items: &[Value]) {
    for item in items {
        let terminal = tokio::time::timeout(
            Duration::from_secs(20),
            client.call(
                "queue.await_job",
                Some(json!({"task_uuid": item["anchor"]})),
            ),
        )
        .await
        .expect("node wait timed out")
        .unwrap();
        assert_eq!(terminal["verdict"], "pass", "{terminal}");
    }
}

async fn runner_output(child: Child) -> std::process::Output {
    tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
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

fn capture(paths: &DaemonPaths, task_uuid: &str) -> String {
    let stdout = fs::read_to_string(
        paths
            .state_dir
            .join("capture")
            .join(format!("{task_uuid}.out")),
    )
    .unwrap_or_default();
    let stderr = fs::read_to_string(
        paths
            .state_dir
            .join("capture")
            .join(format!("{task_uuid}.err")),
    )
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
            let (gh_requests, _path_guard) = install_fake_gh(temp.path());
            let paths = paths(temp.path());
            let mut config = config();
            let config_path = temp.path().join("config.json");
            let six_node_script = temp.path().join("six-node.js");
            fs::write(&six_node_script, six_node_source()).unwrap();
            let github_script = temp.path().join("github-flow.js");
            fs::write(&github_script, one_node_source()).unwrap();
            let divergent_script = temp.path().join("divergent.js");
            fs::write(&divergent_script, divergent_source()).unwrap();
            install_github_flow_producer(
                &mut config,
                vec![
                    env!("CARGO_BIN_EXE_tally").to_owned(),
                    "--config".to_owned(),
                    config_path.display().to_string(),
                    "--socket".to_owned(),
                    paths.socket.display().to_string(),
                    "flow".to_owned(),
                    "run".to_owned(),
                    github_script.display().to_string(),
                    "--args".to_owned(),
                    "{}".to_owned(),
                    "--max-nodes".to_owned(),
                    "1".to_owned(),
                ],
            );
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
                Duration::from_secs(30),
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
                assert_eq!(child["noEnqueue"], true);
                assert!(child.get("relatedTrigger").is_none());
            }
            assert_six_unique_rows(&paths, &parent_uuid);

            // A command-comment producer emits the flow parent once. At runner
            // startup the parent receipt is resolved through query.job and copied
            // onto every child without turning that child into a GitHub job.
            let producer_events = temp.path().join("producer-events");
            let producer_state = temp.path().join("producer-state");
            let producer =
                ProducerEngine::new(&config.producers, producer_events.clone(), &producer_state);
            let observation = github_flow_observation();
            let emitted = match producer
                .emit_gh("github-flow", &observation, chrono::Utc::now())
                .unwrap()
            {
                EmitOutcome::Emitted(path) => path,
                other => panic!("GitHub flow producer did not emit: {other:?}"),
            };
            assert_eq!(
                producer
                    .emit_gh("github-flow", &observation, chrono::Utc::now())
                    .unwrap(),
                EmitOutcome::Duplicate,
                "redelivery of one comment must not launch a second flow parent"
            );
            assert_eq!(
                fs::read_dir(&producer_events)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|entry| entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".producer.json"))
                    .count(),
                1
            );
            let payload: EnqueuePayload =
                serde_json::from_slice(&fs::read(emitted).unwrap()).unwrap();
            assert_eq!(payload.source, Some(EnqueueSource::Gh));
            assert!(!payload.no_enqueue);
            let origin = payload.gh_origin.as_ref().unwrap();
            let expected_related = related_trigger_from_gh_origin(origin).unwrap();
            assert_eq!(expected_related.event_id, "comment-61");
            let expected_parent_uuid = payload.task_uuid.clone().unwrap();
            let github_parent = client
                .call(
                    "queue.enqueue",
                    Some(serde_json::to_value(payload).unwrap()),
                )
                .await
                .unwrap();
            assert_eq!(github_parent["task_uuid"], expected_parent_uuid);
            let github_parent_terminal = tokio::time::timeout(
                Duration::from_secs(30),
                client.call(
                    "queue.await_job",
                    Some(json!({"task_uuid": expected_parent_uuid})),
                ),
            )
            .await
            .expect("GitHub flow parent timed out")
            .unwrap();
            assert_eq!(
                github_parent_terminal["verdict"],
                "pass",
                "{}",
                capture(&paths, &expected_parent_uuid)
            );

            let github_children = wait_for_flow_items(&client, &expected_parent_uuid, 1).await;
            let github_child = &github_children[0];
            assert_eq!(github_child["source"], "orchestrator");
            assert_eq!(github_child["parentTaskUuid"], expected_parent_uuid);
            assert_eq!(github_child["noEnqueue"], true);
            assert_eq!(
                github_child["relatedTrigger"],
                serde_json::to_value(&expected_related).unwrap()
            );
            assert!(github_child["origin"]["value"]["github"].is_null());

            let projected_parent = client
                .call(
                    "query.job",
                    Some(json!({"id": expected_parent_uuid.clone()})),
                )
                .await
                .unwrap();
            assert_eq!(projected_parent["job"]["source"], "gh");
            assert!(projected_parent["job"]["origin"]["value"]["github"].is_object());
            assert_eq!(
                projected_parent["job"]["relatedTrigger"],
                serde_json::to_value(&expected_related).unwrap()
            );

            let events = read_acknowledged_events(&paths.events_dir()).unwrap();
            let durable_parent = events
                .iter()
                .find(|event| event.row.uuid.to_string() == expected_parent_uuid)
                .unwrap();
            assert!(durable_parent.row.gh_origin.is_some());
            let durable_children = events
                .iter()
                .filter(|event| {
                    event
                        .row
                        .orchestration
                        .as_ref()
                        .is_some_and(|orchestration| {
                            orchestration.flow_run_id() == expected_parent_uuid
                        })
                })
                .collect::<Vec<_>>();
            assert_eq!(durable_children.len(), 1);
            assert_eq!(
                durable_children[0].row.related_trigger.as_ref(),
                Some(&expected_related)
            );
            assert_eq!(durable_children[0].row.source, EnqueueSource::Orchestrator);
            assert!(durable_children[0].row.gh_origin.is_none());

            let completed_dir = paths.state_dir.join("producers/gh-completed");
            for _ in 0..400 {
                let completed = fs::read_dir(&completed_dir)
                    .ok()
                    .into_iter()
                    .flatten()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                    .count();
                if completed == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            let gh_requests = fs::read_to_string(&gh_requests)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(gh_requests.len(), 2);
            let completion = gh_requests
                .iter()
                .find(|request| {
                    request["query"]
                        .as_str()
                        .is_some_and(|query| query.contains("TallyCompletionComment"))
                })
                .unwrap();
            assert_eq!(completion["variables"]["itemId"], "I_flow_61");
            let completion_body = completion["variables"]["body"].as_str().unwrap();
            assert!(completion_body.contains(&expected_parent_uuid));
            assert!(!completion_body.contains(github_child["taskUuid"].as_str().unwrap()));

            // The only GitHub culmination names the parent; the child's receipt
            // link is query provenance and does not alter witness bytes.
            let (report, witnesses) = read_verified_records(&paths.witness_path()).unwrap();
            assert!(report.ok);
            let child_witness = witnesses
                .iter()
                .find(|record| {
                    record.orchestration.as_ref().is_some_and(|orchestration| {
                        orchestration.flow_run_id() == expected_parent_uuid
                    })
                })
                .unwrap();
            assert!(serde_json::to_value(child_witness)
                .unwrap()
                .get("relatedTrigger")
                .is_none());

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
            client = rpc(&paths.socket).await;
            assert_eq!(flow_items(&client, RESTARTED_RUN).await.len(), 6);
            assert_six_unique_rows(&paths, RESTARTED_RUN);

            // The same ordinal with changed args reaches the live row's kernel
            // conflict and is surfaced as replay-divergence with both identities.
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
            assert_eq!(event["error"]["code"], "replay-divergence");
            assert_eq!(event["error"]["ordinal"], 0);
            assert!(event["error"]["details"]["expectedHash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));
            assert!(event["error"]["details"]["recordedHash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:"));
            assert_eq!(
                event["error"]["details"]["expectedLabel"],
                "variant-expected"
            );
            assert_eq!(
                event["error"]["details"]["recordedLabel"],
                "variant-recorded"
            );
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
