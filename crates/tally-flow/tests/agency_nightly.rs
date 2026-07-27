use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::rc::Rc;

use serde_json::{json, Value};
use tally_flow::{
    run_script, Admission, ClientError, Disposition, FlowClient, FlowFuture, FlowSubmission,
    NodeResult, RunInspection, RunOptions, VecLifecycleSink, Verdict,
};

const SOURCE: &str = include_str!("../../../examples/flows/agency-nightly.js");
const TASK_IDS: [&str; 6] = ["201", "202", "203", "204", "205", "206"];

#[derive(Clone)]
struct Reply {
    disposition: Disposition,
    result: Value,
}

impl Reply {
    fn new(disposition: Disposition, result: Value) -> Self {
        Self {
            disposition,
            result,
        }
    }

    fn created(result: Value) -> Self {
        Self::new(Disposition::Created, result)
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
        Box::pin(std::future::ready(Ok(RunInspection { script_hash: None })))
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
            .unwrap_or_else(|| panic!("agency flow submitted unexpected ordinal {index}"));
        let task_uuid = submission
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("task-{index}"));
        let terminal = NodeResult {
            task_uuid: task_uuid.clone(),
            verdict: Verdict::Pass,
            exit_code: Some(0),
            witness_seq: u64::try_from(index + 1).expect("test ordinal fits u64"),
            disposition: reply.disposition,
            result: Some(reply.result),
            gates: None,
            error: None,
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

fn args() -> Value {
    json!({
        "repository": "agency/example",
        "checkout": "/srv/agency/example",
        "baseRev": "origin/main",
        "baseBranch": "main",
        "worktreeRoot": "/srv/agency/worktrees",
        "branchPrefix": "agency/nightly",
        "maxWaveSize": 6,
        "reportPath": "/srv/agency/reports/morning.md",
        "driver": {
            "adapter": "agency-driver",
            "program": "/nix/store/00000000000000000000000000000000-agency-driver/bin/agency-driver",
            "runtimeMaxSec": 3600
        }
    })
}

fn entry(task_id: &str) -> Value {
    json!({
        "taskId": task_id,
        "title": format!("[P] Implement task {task_id}"),
        "acceptanceCriteria": [
            {"text": format!("task {task_id} is implemented"), "checked": false},
            {"text": "the focused tests pass", "checked": false}
        ],
        "parallelism": "parallel",
        "files": [format!("src/task-{task_id}.rs")],
        "dependsOn": []
    })
}

fn workspace(task_id: &str) -> Value {
    json!({
        "taskId": task_id,
        "branch": format!("agency/nightly/issue-{task_id}"),
        "worktreePath": format!("/srv/agency/worktrees/issue-{task_id}")
    })
}

fn worklist() -> Value {
    json!({
        "schemaVersion": 1,
        "source": {
            "kind": "github-issues",
            "repository": "agency/example",
            "label": "tally:worklist"
        },
        "baseRev": "0123456789abcdef0123456789abcdef01234567",
        "entries": TASK_IDS.map(entry),
        "wave": TASK_IDS,
        "workspaces": TASK_IDS.map(workspace)
    })
}

fn implementation(task_id: &str) -> Value {
    json!({
        "taskId": task_id,
        "branch": format!("agency/nightly/issue-{task_id}"),
        "head": format!("{task_id:0<40}"),
        "summary": format!("Implemented the complete bounded change for task {task_id}."),
        "tests": ["cargo test -p example: pass"]
    })
}

fn review(task_id: &str) -> Value {
    json!({
        "taskId": task_id,
        "reviewedHead": format!("{task_id:0<40}"),
        "verdict": "approve",
        "summary": format!("Task {task_id} satisfies its acceptance criteria."),
        "findings": []
    })
}

fn culmination() -> Value {
    json!({
        "status": "ready",
        "reportPath": "/srv/agency/reports/morning.md",
        "pullRequests": TASK_IDS.map(|task_id| json!({
            "taskId": task_id,
            "branch": format!("agency/nightly/issue-{task_id}"),
            "status": "created",
            "url": format!("https://github.com/agency/example/pull/{task_id}")
        }))
    })
}

fn ok(value: Value) -> Value {
    json!({"ok": true, "value": value})
}

fn complete_replies() -> Vec<Reply> {
    let mut replies = vec![Reply::created(ok(worklist()))];
    replies.extend(
        TASK_IDS
            .iter()
            .map(|task_id| Reply::created(implementation(task_id))),
    );
    replies.extend(
        TASK_IDS
            .iter()
            .map(|task_id| Reply::created(review(task_id))),
    );
    replies.push(Reply::created(ok(culmination())));
    replies
}

fn run(client: Rc<TestClient>) -> Result<tally_flow::RunReport, Box<tally_flow::FlowError>> {
    let mut options = RunOptions::new("agency-run", args());
    options.max_nodes = 14;
    run_script(
        SOURCE,
        Some(Path::new("examples/flows/agency-nightly.js")),
        client,
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .map_err(Box::new)
}

#[test]
fn six_task_wave_has_distinct_worktrees_cross_harness_review_and_one_culmination() {
    let client = TestClient::new(complete_replies());
    let report = run(client.clone()).expect("agency wave succeeds");

    assert_eq!(
        report.final_value.as_ref().unwrap()["wave"],
        json!(TASK_IDS)
    );
    assert_eq!(
        report.final_value.as_ref().unwrap()["culmination"]["status"],
        "ready"
    );

    let submissions = client.submissions.borrow();
    assert_eq!(submissions.len(), 14);
    assert_eq!(
        submissions[0].orchestration.node_label.as_deref(),
        Some("agency-worklist")
    );
    assert_eq!(
        submissions[0].spec.adapter.as_deref(),
        Some("agency-driver")
    );
    assert_eq!(submissions[0].spec.pools, ["agency-control"]);
    assert_eq!(
        submissions[0].spec.brief.as_ref().unwrap()["source"]["label"],
        "tally:worklist"
    );

    let implementations = &submissions[1..7];
    for (submission, task_id) in implementations.iter().zip(TASK_IDS) {
        assert_eq!(submission.spec.adapter.as_deref(), Some("codex"));
        assert_eq!(submission.spec.pools, ["codex-window"]);
        assert_eq!(submission.spec.priority.as_deref(), Some("low"));
        assert_eq!(
            submission.dedup_key,
            format!("flow:agency-run:k:implementation-{task_id}")
        );
        assert_eq!(
            submission.spec.workspace.as_ref().unwrap()["branch"],
            format!("agency/nightly/issue-{task_id}")
        );
        assert_eq!(
            submission.spec.workspace.as_ref().unwrap()["worktreePath"],
            format!("/srv/agency/worktrees/issue-{task_id}")
        );
    }
    let distinct_worktrees = implementations
        .iter()
        .map(|submission| {
            submission.spec.workspace.as_ref().unwrap()["worktreePath"]
                .as_str()
                .unwrap()
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(distinct_worktrees.len(), 6);

    let reviews = &submissions[7..13];
    for ((submission, implementation), task_id) in reviews.iter().zip(implementations).zip(TASK_IDS)
    {
        assert_eq!(submission.spec.adapter.as_deref(), Some("claude-code"));
        assert_eq!(submission.spec.pools, ["claude-window"]);
        assert_eq!(submission.spec.priority.as_deref(), Some("low"));
        assert_eq!(
            submission.dedup_key,
            format!("flow:agency-run:k:review-{task_id}")
        );
        assert_eq!(submission.spec.workspace, implementation.spec.workspace);
        assert!(submission.spec.brief.as_ref().unwrap()["mission"]
            .as_str()
            .unwrap()
            .contains("The implementing harness never certifies its own work"));
    }

    let culmination_submission = &submissions[13];
    assert_eq!(
        culmination_submission.orchestration.node_label.as_deref(),
        Some("agency-culminate")
    );
    assert_eq!(culmination_submission.spec.pools, ["agency-control"]);
    assert_eq!(
        culmination_submission.spec.evidence,
        [
            "exit:0",
            "artifact:/srv/agency/reports/morning.md",
            "hash:sha256"
        ]
    );
    assert_eq!(
        culmination_submission.spec.brief.as_ref().unwrap()["tasks"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
}

#[test]
fn restart_mid_wave_reuses_finished_implementations_and_attaches_the_in_flight_one() {
    let mut replies = vec![Reply::new(Disposition::Reused, ok(worklist()))];
    for (index, task_id) in TASK_IDS.iter().enumerate() {
        let disposition = match index {
            0 | 1 => Disposition::Reused,
            2 => Disposition::Attached,
            _ => Disposition::Created,
        };
        replies.push(Reply::new(disposition, implementation(task_id)));
    }
    replies.extend(
        TASK_IDS
            .iter()
            .map(|task_id| Reply::created(review(task_id))),
    );
    replies.push(Reply::created(ok(culmination())));
    let client = TestClient::new(replies);

    run(client.clone()).expect("restarted wave reaches culmination");
    assert_eq!(
        &client.dispositions.borrow()[0..7],
        [
            Disposition::Reused,
            Disposition::Reused,
            Disposition::Reused,
            Disposition::Attached,
            Disposition::Created,
            Disposition::Created,
            Disposition::Created,
        ]
    );
    let submissions = client.submissions.borrow();
    assert_eq!(
        submissions[3].dedup_key,
        "flow:agency-run:k:implementation-203"
    );
    assert_eq!(
        submissions
            .last()
            .unwrap()
            .orchestration
            .node_label
            .as_deref(),
        Some("agency-culminate")
    );
}

#[test]
fn invalid_labeled_issue_is_rejected_with_the_drivers_typed_error() {
    let client = TestClient::new(vec![Reply::created(json!({
        "ok": false,
        "error": {
            "code": "worklist-acceptance-missing",
            "message": "issue #209 has no task-list checklist under ## Acceptance",
            "details": {"taskId": "209"}
        }
    }))]);

    let error = run(client.clone()).expect_err("invalid worklist must fail closed");
    assert_eq!(error.name, "AgencyDriverError");
    assert_eq!(error.code, "worklist-acceptance-missing");
    assert_eq!(error.details["taskId"], "209");
    assert_eq!(client.submissions.borrow().len(), 1);
}
