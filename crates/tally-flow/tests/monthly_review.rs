use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::rc::Rc;

use serde_json::{json, Value};
use tally_flow::{
    run_script, Admission, Catalog, CatalogMember, ClientError, Disposition, FlowClient,
    FlowFuture, FlowSubmission, NodeFailure, NodeResult, RunInspection, RunOptions,
    VecLifecycleSink, Verdict,
};

const SOURCE: &str = include_str!("../../../examples/flows/monthly-review.js");

#[derive(Clone)]
struct Reply {
    disposition: Disposition,
    verdict: Verdict,
    result: Option<Value>,
}

impl Reply {
    fn pass(result: Value) -> Self {
        Self {
            disposition: Disposition::Created,
            verdict: Verdict::Pass,
            result: Some(result),
        }
    }

    fn terminal(result: Value) -> Self {
        Self {
            disposition: Disposition::Terminal,
            verdict: Verdict::Pass,
            result: Some(result),
        }
    }
}

struct TestClient {
    replies: RefCell<VecDeque<Reply>>,
    submissions: RefCell<Vec<FlowSubmission>>,
    dispositions: RefCell<Vec<Disposition>>,
    terminals: RefCell<BTreeMap<String, NodeResult>>,
}

impl TestClient {
    fn new(replies: Vec<Reply>) -> Rc<Self> {
        Rc::new(Self {
            replies: RefCell::new(replies.into()),
            submissions: RefCell::default(),
            dispositions: RefCell::default(),
            terminals: RefCell::default(),
        })
    }
}

impl FlowClient for TestClient {
    fn inspect_run<'a>(
        &'a self,
        _flow_run_id: &'a str,
    ) -> FlowFuture<'a, Result<RunInspection, ClientError>> {
        Box::pin(std::future::ready(Ok(RunInspection::default())))
    }

    fn submit<'a>(
        &'a self,
        submission: FlowSubmission,
    ) -> FlowFuture<'a, Result<Admission, ClientError>> {
        let index = self.submissions.borrow().len();
        let reply = self
            .replies
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| panic!("monthly review submitted unexpected ordinal {index}"));
        let task_uuid = submission
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("task-{index}"));
        let task_ref = submission.orchestration.task_ref.clone();
        let terminal = NodeResult {
            task_uuid: task_uuid.clone(),
            task_ref: task_ref.clone(),
            verdict: reply.verdict,
            exit_code: Some(if reply.verdict.is_pass() { 0 } else { 1 }),
            stderr_excerpt: None,
            stderr_truncated: None,
            witness_seq: u64::try_from(index + 1).expect("test ordinal fits u64"),
            disposition: reply.disposition,
            result: reply.result,
            gates: None,
            error: (!reply.verdict.is_pass()).then(|| NodeFailure {
                code: "worker-failed".to_owned(),
                message: "worker failed".to_owned(),
                details: None,
            }),
        };
        let inline = matches!(
            reply.disposition,
            Disposition::Reused | Disposition::Substituted | Disposition::Terminal
        )
        .then(|| terminal.clone());
        self.terminals
            .borrow_mut()
            .insert(task_uuid.clone(), terminal);
        self.dispositions.borrow_mut().push(reply.disposition);
        self.submissions.borrow_mut().push(submission.clone());
        Box::pin(std::future::ready(Ok(Admission {
            schema_version: 1,
            disposition: reply.disposition,
            task_uuid,
            task_ref,
            payload_hash: submission.payload_hash,
            attempt: 1,
            terminal: inline,
            recorded_label: None,
            reused_rejected: None,
        })))
    }

    fn await_terminal<'a>(
        &'a self,
        task_uuid: &'a str,
        _attempt: u32,
    ) -> FlowFuture<'a, Result<NodeResult, ClientError>> {
        let result = self
            .terminals
            .borrow()
            .get(task_uuid)
            .cloned()
            .ok_or_else(|| ClientError::new("missing-terminal", task_uuid));
        Box::pin(std::future::ready(result))
    }
}

fn drv(name: &str) -> Value {
    json!({
        "drvPath": format!("/nix/store/00000000000000000000000000000000-{name}.drv"),
        "outputs": [{
            "name": "out",
            "path": format!("/nix/store/00000000000000000000000000000000-{name}")
        }]
    })
}

fn capture() -> Value {
    json!({
        "period": "2026-07",
        "changedCount": 2,
        "dotfilesCommit": "0123456789abcdef0123456789abcdef01234567",
        "runDir": "/tmp/monthly-review/2026-07.run",
        "receiptPath": "/tmp/monthly-review/last-run.json",
        "commentaryPath": "/tmp/monthly-review/2026-07.run/pr-commentary.md",
        "preparation": drv("prepare")
    })
}

fn enriched() -> Value {
    json!({
        "evidenceDigest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "provider": "llama-swap",
        "modelId": "qwen",
        "endpoint": "http://127.0.0.1:8080/v1",
        "modelTimeoutSec": 3600,
        "promptPath": "/nix/store/00000000000000000000000000000000-review.md",
        "evidencePath": "/nix/store/00000000000000000000000000000000-enriched/evidence.md",
        "contextPath": "/nix/store/00000000000000000000000000000000-enriched/context.md",
        "hfMetadataPath": "/nix/store/00000000000000000000000000000000-enriched/hf-metadata.md",
        "enrichment": drv("enrich")
    })
}

fn finalization() -> Value {
    json!({
        "commentaryPath": "/tmp/monthly-review/2026-07.run/pr-commentary.md",
        "finalization": drv("finalize")
    })
}

fn publication() -> Value {
    json!({
        "status": "published",
        "period": "2026-07",
        "branch": "automation/local-ai-review-2026-07",
        "title": "chore(local-ai): 2026-07 source review",
        "changedFile": "pkgs/local-ai-monthly/sources.json",
        "prUrl": "https://github.com/mecattaf/dotfiles/pull/123"
    })
}

fn comment(member: &str) -> Value {
    json!(format!(
        "{member} found concrete source and model-roster changes that belong in the monthly review."
    ))
}

fn reduction() -> Value {
    json!({
        "commentary": "The attributed reviewers agree on the pin updates while retaining one concrete disagreement.",
        "conclusions": [{
            "conclusion": "Advance the two reviewed source pins.",
            "support": ["qwen", "llama"],
            "conflict": ["mistral"]
        }]
    })
}

fn catalog() -> Catalog {
    Catalog {
        version: 1,
        members: [
            ("qwen", "alibaba"),
            ("llama", "meta"),
            ("mistral", "mistral"),
        ]
        .into_iter()
        .map(|(id, maker)| CatalogMember {
            id: id.to_owned(),
            family: id.to_owned(),
            maker: maker.to_owned(),
            classes: vec!["pooled-strongest".to_owned()],
            adapter: "pi".to_owned(),
            pools: vec!["coordinator-gpu".to_owned()],
            launch: json!({"model": id}),
            architecture: None,
            fine_tune: None,
            backend: None,
            modality: None,
            role: None,
            status: None,
            evidence: None,
            hosts: Vec::new(),
            base_checkpoint: None,
            supersedes: None,
            superseded_by: None,
            notes: None,
        })
        .collect(),
    }
}

fn options() -> RunOptions {
    let mut options = RunOptions::new(
        "monthly-run",
        json!({
            "minimumValid": 2,
            "publish": true,
            "dotfilesUrl": "https://github.com/mecattaf/dotfiles.git",
            "baseBranch": "main",
            "period": "2026-07",
            "driver": {
                "adapter": "monthly-review-driver",
                "program": "/nix/store/00000000000000000000000000000000-driver/bin/driver",
                "stateDir": "/tmp/monthly-review",
                "receiptPath": "/tmp/monthly-review/last-run.json",
                "runtimeMaxSec": 43200
            }
        }),
    );
    options.max_nodes = 16;
    options.catalog = Some(catalog());
    options.catalog_hash = Some("sha256:monthly-catalog".to_owned());
    options
}

fn run(client: Rc<TestClient>) -> Result<tally_flow::RunReport, Box<tally_flow::FlowError>> {
    run_script(
        SOURCE,
        Some(Path::new("examples/flows/monthly-review.js")),
        client,
        Rc::new(VecLifecycleSink::default()),
        options(),
    )
    .map_err(Box::new)
}

#[test]
fn pooled_review_repairs_one_member_and_publishes_attributed_dissent() {
    let client = TestClient::new(vec![
        Reply::pass(capture()),
        Reply::pass(json!({"built": "prepare"})),
        Reply::pass(enriched()),
        Reply::pass(json!({"built": "enrich"})),
        Reply::pass(comment("qwen")),
        Reply::pass(json!("bad")),
        Reply::pass(comment("mistral")),
        Reply::pass(comment("llama repair")),
        Reply::pass(reduction()),
        Reply::pass(finalization()),
        Reply::pass(json!({"built": "finalize"})),
        Reply::pass(publication()),
    ]);

    let report = run(client.clone()).expect("monthly review succeeds");
    assert_eq!(
        report.final_value.as_ref().unwrap()["publication"],
        publication()
    );
    assert_eq!(
        report.final_value.as_ref().unwrap()["quorum"]["valid"],
        json!(["qwen", "llama", "mistral"])
    );

    let submissions = client.submissions.borrow();
    let base = "local-ai-judge-2026-07-aaaaaaaaaaaaaaaaaaaa";
    let member_keys = submissions
        .iter()
        .filter(|submission| {
            submission
                .orchestration
                .node_label
                .as_deref()
                .is_some_and(|label| label.starts_with("review-") || label.starts_with("repair-"))
        })
        .map(|submission| submission.dedup_key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        member_keys,
        [
            base,
            "local-ai-judge-2026-07-aaaaaaaaaaaaaaaaaaaa-llama",
            "local-ai-judge-2026-07-aaaaaaaaaaaaaaaaaaaa-mistral",
            "local-ai-judge-2026-07-aaaaaaaaaaaaaaaaaaaa-llama@1",
        ]
    );
    let finalize_brief = submissions
        .iter()
        .find(|submission| {
            submission.orchestration.node_label.as_deref() == Some("monthly-review-finalize")
        })
        .and_then(|submission| submission.spec.brief.as_ref())
        .expect("finalize brief");
    let commentary = finalize_brief["commentary"]
        .as_str()
        .expect("rendered commentary");
    for required in [
        "## Per-model reviews",
        "### qwen",
        "### llama",
        "### mistral",
        "## Dissent ledger",
        "Support: qwen, llama",
        "Conflict: mistral",
    ] {
        assert!(commentary.contains(required), "missing {required:?}");
    }
}

#[test]
fn pooled_review_fails_below_quorum_after_exactly_one_repair_per_member() {
    let failure = json!({
        "status": "failed",
        "receiptPath": "/tmp/monthly-review/last-run.json"
    });
    let client = TestClient::new(vec![
        Reply::pass(capture()),
        Reply::pass(json!({"built": "prepare"})),
        Reply::pass(enriched()),
        Reply::pass(json!({"built": "enrich"})),
        Reply::pass(json!("bad")),
        Reply::pass(json!("bad")),
        Reply::pass(json!("bad")),
        Reply::pass(json!("bad")),
        Reply::pass(json!("bad")),
        Reply::pass(json!("bad")),
        Reply::pass(failure),
    ]);

    let error = run(client.clone()).expect_err("quorum failure is terminal");
    assert_eq!(error.code, "quorum-not-met");
    let submissions = client.submissions.borrow();
    let repair_keys = submissions
        .iter()
        .filter(|submission| {
            submission
                .orchestration
                .node_label
                .as_deref()
                .is_some_and(|label| label.starts_with("repair-"))
        })
        .map(|submission| submission.dedup_key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        repair_keys,
        [
            "local-ai-judge-2026-07-aaaaaaaaaaaaaaaaaaaa-qwen@1",
            "local-ai-judge-2026-07-aaaaaaaaaaaaaaaaaaaa-llama@1",
            "local-ai-judge-2026-07-aaaaaaaaaaaaaaaaaaaa-mistral@1",
        ]
    );
    assert_eq!(
        submissions
            .iter()
            .filter(|submission| {
                submission
                    .orchestration
                    .node_label
                    .as_deref()
                    .is_some_and(|label| label == "dissent-reducer")
            })
            .count(),
        0
    );
}

#[test]
fn killed_runner_prefix_reuses_completed_members_without_reinference() {
    let client = TestClient::new(vec![
        Reply::terminal(capture()),
        Reply::terminal(json!({"built": "prepare"})),
        Reply::terminal(enriched()),
        Reply::terminal(json!({"built": "enrich"})),
        Reply::terminal(comment("qwen")),
        Reply::terminal(comment("llama")),
        Reply::terminal(comment("mistral")),
        Reply::pass(reduction()),
        Reply::pass(finalization()),
        Reply::pass(json!({"built": "finalize"})),
        Reply::pass(publication()),
    ]);

    let report = run(client.clone()).expect("restarted runner reaches the frontier");
    assert_eq!(
        report.final_value.as_ref().unwrap()["publication"]["status"],
        "published"
    );
    let submissions = client.submissions.borrow();
    let dispositions = client.dispositions.borrow();
    let initial_members = submissions
        .iter()
        .enumerate()
        .filter(|(_, submission)| {
            submission
                .orchestration
                .node_label
                .as_deref()
                .is_some_and(|label| label.starts_with("review-"))
        })
        .map(|(index, _)| dispositions[index])
        .collect::<Vec<_>>();
    assert_eq!(
        initial_members,
        [
            Disposition::Terminal,
            Disposition::Terminal,
            Disposition::Terminal,
        ]
    );
    let reducer_index = submissions
        .iter()
        .position(|submission| {
            submission.orchestration.node_label.as_deref() == Some("dissent-reducer")
        })
        .expect("reducer is the live frontier");
    assert_eq!(dispositions[reducer_index], Disposition::Created);
}
