//! The agency nightly wave, driven end to end against a scripted client.
//!
//! What these tests pin is the shape the wave ruling asks for: one worktree,
//! branch, and node key per task; a cross-harness review that never certifies;
//! and a culmination that runs whatever the wave did — including when a task
//! fails, and including when the worklist itself fails.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;
use std::rc::Rc;

use serde_json::{json, Value};
use tally_flow::{
    run_script, Admission, ClientError, Disposition, FlowClient, FlowFuture, FlowSubmission,
    NodeFailure, NodeResult, RunInspection, RunOptions, VecLifecycleSink, Verdict,
};

const SOURCE: &str = include_str!("../../../examples/flows/agency-nightly.js");
const TASK_IDS: [&str; 6] = [
    "worklist-node",
    "settle-mode",
    "repair-headroom",
    "fanout-cap",
    "crash-resume",
    "morning-report",
];

#[derive(Clone)]
struct Reply {
    disposition: Disposition,
    verdict: Verdict,
    result: Option<Value>,
    error: Option<NodeFailure>,
}

impl Reply {
    fn new(disposition: Disposition, result: Value) -> Self {
        Self {
            disposition,
            verdict: Verdict::Pass,
            result: Some(result),
            error: None,
        }
    }

    fn created(result: Value) -> Self {
        Self::new(Disposition::Created, result)
    }

    /// A node the daemon witnessed as failed. With `settle: true` the flow sees
    /// this as data rather than as a rejected promise.
    fn failed(code: &str, message: &str) -> Self {
        Self {
            disposition: Disposition::Created,
            verdict: Verdict::Failed,
            result: None,
            error: Some(NodeFailure {
                code: code.to_owned(),
                message: message.to_owned(),
                details: None,
            }),
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

    fn labelled(&self, label: &str) -> FlowSubmission {
        self.submissions
            .borrow()
            .iter()
            .find(|submission| submission.orchestration.node_label.as_deref() == Some(label))
            .cloned()
            .unwrap_or_else(|| panic!("no submission labelled {label}"))
    }

    fn labels(&self) -> Vec<String> {
        self.submissions
            .borrow()
            .iter()
            .map(|submission| {
                submission
                    .orchestration
                    .node_label
                    .clone()
                    .unwrap_or_default()
            })
            .collect()
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
            .unwrap_or_else(|| panic!("agency flow submitted unexpected ordinal {index}"));
        let task_uuid = submission
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("task-{index}"));
        let terminal = NodeResult {
            task_uuid: task_uuid.clone(),
            verdict: reply.verdict,
            exit_code: Some(i32::from(!reply.verdict.is_pass())),
            witness_seq: u64::try_from(index + 1).expect("test ordinal fits u64"),
            disposition: reply.disposition,
            result: reply.result,
            gates: None,
            error: reply.error,
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

fn wave() -> Value {
    Value::Array(
        TASK_IDS
            .iter()
            .map(|task_id| {
                json!({
                    "taskId": task_id,
                    "title": format!("Land {task_id}"),
                    "mission": format!("Carry {task_id} to completion inside its worktree."),
                    "acceptanceCriteria": [format!("{task_id} is implemented and tested")]
                })
            })
            .collect::<Vec<_>>(),
    )
}

fn args() -> Value {
    json!({
        "repository": "agency/example",
        "checkout": "/srv/agency/example",
        "baseRev": "origin/main",
        "baseBranch": "main",
        "worktreeRoot": "/srv/agency/worktrees",
        "branchPrefix": "agency/nightly",
        "reportPath": "/srv/agency/reports/morning.md",
        "driver": {
            "adapter": "agency-driver",
            "program": "/nix/store/00000000000000000000000000000000-driver/bin/driver",
            "runtimeMaxSec": 900
        },
        "implementationRuntimeMaxSec": 14400,
        "reviewRuntimeMaxSec": 7200,
        "wave": wave()
    })
}

fn task(task_id: &str) -> Value {
    json!({
        "taskId": task_id,
        "title": format!("Land {task_id}"),
        "mission": format!("Carry {task_id} to completion inside its worktree."),
        "acceptanceCriteria": [format!("{task_id} is implemented and tested")]
    })
}

fn workspace(task_id: &str) -> Value {
    json!({
        "taskId": task_id,
        "branch": format!("agency/nightly/{task_id}"),
        "worktreePath": format!("/srv/agency/worktrees/{task_id}")
    })
}

fn worklist() -> Value {
    json!({
        "schemaVersion": 1,
        "repository": "agency/example",
        "baseRev": "0123456789abcdef0123456789abcdef01234567",
        "tasks": TASK_IDS.iter().map(|id| task(id)).collect::<Vec<_>>(),
        "workspaces": TASK_IDS.iter().map(|id| workspace(id)).collect::<Vec<_>>()
    })
}

fn head(task_id: &str) -> String {
    let digit = char::from_digit(
        u32::try_from(
            TASK_IDS
                .iter()
                .position(|candidate| *candidate == task_id)
                .expect("task id belongs to the wave"),
        )
        .expect("wave index fits u32"),
        10,
    )
    .expect("wave index is a decimal digit");
    std::iter::repeat_n(digit, 40).collect()
}

fn implementation(task_id: &str) -> Value {
    json!({
        "taskId": task_id,
        "branch": format!("agency/nightly/{task_id}"),
        "head": head(task_id),
        "summary": format!("Implemented {task_id}."),
        "tests": ["cargo test --workspace: pass"]
    })
}

fn review(task_id: &str) -> Value {
    json!({
        "taskId": task_id,
        "reviewedHead": head(task_id),
        "verdict": "approve",
        "summary": format!("Reviewed {task_id} against its acceptance criteria."),
        "findings": []
    })
}

fn culmination(status: &str, pull_requests: usize, failures: Vec<Value>) -> Value {
    json!({
        "status": status,
        "reportPath": "/srv/agency/reports/morning.md",
        "pullRequests": TASK_IDS.iter().take(pull_requests).map(|task_id| json!({
            "taskId": task_id,
            "branch": format!("agency/nightly/{task_id}"),
            "status": "created",
            "url": format!("https://example.test/pull/{task_id}")
        })).collect::<Vec<_>>(),
        "failures": failures
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
    replies.push(Reply::created(ok(culmination("ready", 6, Vec::new()))));
    replies
}

fn run(client: Rc<TestClient>) -> Result<tally_flow::RunReport, Box<tally_flow::FlowError>> {
    let mut options = RunOptions::new("agency-run", args());
    options.max_nodes = 20;
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

    let final_value = report.final_value.as_ref().unwrap();
    assert_eq!(final_value["wave"], json!(TASK_IDS));
    assert_eq!(final_value["culmination"]["status"], "ready");
    assert_eq!(
        final_value["baseRev"],
        "0123456789abcdef0123456789abcdef01234567"
    );

    // 1 worklist + 6 implementations + 6 reviews + 1 culmination, well inside
    // the declared maxNodes of 20 and its repair headroom.
    assert_eq!(client.submissions.borrow().len(), 14);

    let worklist_submission = client.labelled("agency-worklist");
    assert_eq!(
        worklist_submission.spec.adapter.as_deref(),
        Some("agency-driver")
    );
    assert_eq!(worklist_submission.spec.pools, ["agency-control"]);
    assert_eq!(worklist_submission.spec.priority.as_deref(), Some("low"));
    // The wave travels in the brief. There is no worklist source to query.
    assert_eq!(
        worklist_submission.spec.brief.as_ref().unwrap()["wave"],
        wave()
    );

    let mut worktrees = BTreeSet::new();
    let mut branches = BTreeSet::new();
    for task_id in TASK_IDS {
        let implementation = client.labelled(&format!("implement-{task_id}"));
        assert_eq!(implementation.spec.adapter.as_deref(), Some("codex"));
        assert_eq!(implementation.spec.pools, ["codex-window"]);
        assert_eq!(implementation.spec.priority.as_deref(), Some("low"));
        assert_eq!(implementation.spec.runtime_max_sec, Some(14400));
        assert_eq!(
            implementation.dedup_key,
            format!("flow:agency-run:k:implement-{task_id}")
        );
        let workspace = implementation.spec.workspace.as_ref().unwrap();
        assert_eq!(workspace["branch"], format!("agency/nightly/{task_id}"));
        assert_eq!(
            workspace["worktreePath"],
            format!("/srv/agency/worktrees/{task_id}")
        );
        worktrees.insert(workspace["worktreePath"].as_str().unwrap().to_owned());
        branches.insert(workspace["branch"].as_str().unwrap().to_owned());

        let review = client.labelled(&format!("review-{task_id}"));
        assert_eq!(review.spec.adapter.as_deref(), Some("claude-code"));
        assert_eq!(review.spec.pools, ["claude-window"]);
        assert_eq!(review.spec.priority.as_deref(), Some("low"));
        assert_eq!(review.spec.runtime_max_sec, Some(7200));
        assert_eq!(
            review.dedup_key,
            format!("flow:agency-run:k:review-{task_id}")
        );
        // Same worktree, different harness: the reviewer reads what codex wrote.
        assert_eq!(review.spec.workspace, implementation.spec.workspace);
        let mission = review.spec.brief.as_ref().unwrap()["mission"]
            .as_str()
            .unwrap();
        assert!(mission.contains("You are a finder, not a certifier"));
        assert!(mission.contains("Do not modify the worktree"));
    }
    assert_eq!(worktrees.len(), 6);
    assert_eq!(branches.len(), 6);

    let culmination_submission = client.labelled("agency-culminate");
    assert_eq!(culmination_submission.spec.pools, ["agency-control"]);
    assert_eq!(
        culmination_submission.spec.evidence,
        [
            "exit:0",
            "artifact:/srv/agency/reports/morning.md",
            "hash:sha256"
        ]
    );
    let brief = culmination_submission.spec.brief.as_ref().unwrap();
    assert_eq!(brief["tasks"].as_array().unwrap().len(), 6);
    assert!(brief["worklistError"].is_null());
    // No human gate anywhere in the middle: the culmination is the only node
    // between the wave and the operator's morning.
    assert_eq!(
        client
            .labels()
            .iter()
            .filter(|label| label.as_str() == "agency-culminate")
            .count(),
        1
    );
}

#[test]
fn one_failed_task_does_not_suppress_the_culmination() {
    let failing = TASK_IDS[2];
    let mut replies = vec![Reply::created(ok(worklist()))];
    for task_id in TASK_IDS {
        if task_id == failing {
            replies.push(Reply::failed(
                "worker-failed",
                "the implementation harness exited nonzero",
            ));
        } else {
            replies.push(Reply::created(implementation(task_id)));
        }
    }
    // The failed task has no review: its chain stops after the implementation.
    replies.extend(
        TASK_IDS
            .iter()
            .filter(|task_id| **task_id != failing)
            .map(|task_id| Reply::created(review(task_id))),
    );
    replies.push(Reply::created(ok(culmination(
        "partial",
        5,
        vec![json!({
            "taskId": failing,
            "stage": "implementation",
            "code": "node-failed",
            "message": "failed: node returned no structured result"
        })],
    ))));
    let client = TestClient::new(replies);

    let report = run(client.clone()).expect("a failed task must not fail the wave");
    assert_eq!(
        report.final_value.as_ref().unwrap()["culmination"]["status"],
        "partial"
    );

    // 1 worklist + 6 implementations + 5 reviews + 1 culmination.
    assert_eq!(client.submissions.borrow().len(), 13);
    assert!(!client.labels().contains(&format!("review-{failing}")));

    let brief = client
        .labelled("agency-culminate")
        .spec
        .brief
        .clone()
        .unwrap();
    let tasks = brief["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 6);
    let failed = tasks
        .iter()
        .find(|entry| entry["task"]["taskId"] == failing)
        .expect("the failed task still reaches the culmination");
    assert_eq!(failed["failure"]["stage"], "implementation");
    // The witnessed verdict leads. The engine's resultSchema check replaces the
    // daemon's NodeFailure on a node that failed before producing a result, so
    // the message describes the symptom; the verdict is what stays true.
    assert_eq!(failed["failure"]["code"], "node-failed");
    assert_eq!(
        failed["failure"]["message"],
        "failed: node returned no structured result"
    );
    assert!(failed["implementation"].is_null());
    assert!(failed["review"].is_null());
    // Every other task arrives whole.
    assert_eq!(
        tasks
            .iter()
            .filter(|entry| entry["failure"].is_null() && !entry["review"].is_null())
            .count(),
        5
    );
}

#[test]
fn a_worklist_that_cannot_prepare_the_wave_still_reaches_the_culmination() {
    let client = TestClient::new(vec![
        Reply::created(json!({
            "ok": false,
            "error": {
                "code": "worklist-worktree-conflict",
                "message": "existing path for task 'settle-mode' is not a worktree of the configured checkout",
                "details": {"taskId": "settle-mode"}
            }
        })),
        Reply::created(ok(json!({
            "status": "worklist-failed",
            "reportPath": "/srv/agency/reports/morning.md",
            "pullRequests": [],
            "failures": []
        }))),
    ]);

    let report = run(client.clone()).expect("a failed worklist still owes a morning report");
    let final_value = report.final_value.as_ref().unwrap();
    assert_eq!(final_value["culmination"]["status"], "worklist-failed");
    assert!(final_value["baseRev"].is_null());
    assert_eq!(final_value["wave"], json!([]));

    assert_eq!(client.submissions.borrow().len(), 2);
    let brief = client
        .labelled("agency-culminate")
        .spec
        .brief
        .clone()
        .unwrap();
    assert_eq!(brief["worklistError"]["code"], "worklist-worktree-conflict");
    assert_eq!(brief["tasks"], json!([]));
    assert!(brief["baseRev"].is_null());
}

#[test]
fn a_culmination_that_cannot_report_ends_the_run() {
    let mut replies = complete_replies();
    let last = replies.len() - 1;
    replies[last] = Reply::created(json!({
        "ok": false,
        "error": {
            "code": "culmination-push-failed",
            "message": "git exited 128",
            "details": {}
        }
    }));
    let client = TestClient::new(replies);

    let error = run(client.clone()).expect_err("no report means no run");
    assert_eq!(error.name, "AgencyCulminationError");
    assert_eq!(error.code, "culmination-push-failed");
    assert_eq!(client.submissions.borrow().len(), 14);
}

#[test]
fn restart_mid_wave_reuses_finished_tasks_and_attaches_the_in_flight_one() {
    // The same flowRunId, re-entered: whatever already reached a terminal
    // witness comes back Reused, the node still running comes back Attached,
    // and the rest are created for the first time. Nothing in the script knows.
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
    replies.push(Reply::created(ok(culmination("ready", 6, Vec::new()))));
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
    // Keys survive the restart because they derive from the witnessed worklist.
    assert_eq!(
        client
            .labelled(&format!("implement-{}", TASK_IDS[2]))
            .dedup_key,
        format!("flow:agency-run:k:implement-{}", TASK_IDS[2])
    );
    assert_eq!(
        client
            .submissions
            .borrow()
            .last()
            .unwrap()
            .orchestration
            .node_label
            .as_deref(),
        Some("agency-culminate")
    );
}
