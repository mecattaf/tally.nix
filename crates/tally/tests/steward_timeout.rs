//! The judge's own death, bound at the flow: a steward node that projects
//! nothing blocks its task and the pass survives.
//!
//! Three defects arrived in one incident at the eta C1 re-witness (2026-08-18,
//! `specs/eta/evidence/run-log.md`): the checkpoint's diagnosis node hit a
//! 120-second unit budget while waiting on one model call, the missing
//! `finalMessage` projection surfaced as a `result-schema-mismatch`
//! `FlowResultError`, and that error killed the whole pass — a dead judge took
//! down a frontier that had nothing to do with it.
//!
//! The driver half of the repair is proven in Python (`action_steer` with a
//! `diagnosisUnavailable` brief: forced `blocked`, composed receipt, refused
//! proposal). None of that proves the flow ever *survives* the death. Restore
//! `settle: false` on the diagnosis node and every driver test stays green
//! while one dead steward ends the pass again; this file is the binding.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use serde_json::{json, Value};
use tally_core::campaign_contract::DEFAULT_STEWARD_DIAGNOSIS_RUNTIME_MAX_SEC;
use tally_flow::{
    run_script, Admission, ClientError, Disposition, FlowClient, FlowFuture, FlowSubmission,
    NodeFailure, NodeResult, RunInspection, RunOptions, VecLifecycleSink, Verdict,
};

const SOURCE: &str = include_str!("../../../examples/flows/spec-build.js");
const GATE_ID: &str = "chapter-gate";
const REV: &str = "0123456789abcdef0123456789abcdef01234567";
const PROVEN: &str = "fedcba9876543210fedcba9876543210fedcba98";
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[derive(Clone)]
struct Reply {
    verdict: Verdict,
    result: Option<Value>,
    error: Option<NodeFailure>,
}

impl Reply {
    fn passed(result: Value) -> Self {
        Self {
            verdict: Verdict::Pass,
            result: Some(result),
            error: None,
        }
    }

    /// A node the daemon witnessed as failed with nothing projected — the
    /// exact shape of a steward killed at its unit budget mid model call.
    fn projected_nothing(code: &str, message: &str) -> Self {
        Self {
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

/// Replies keyed by node label, so the fixture cannot answer the wrong node
/// when the flow's node order shifts.
struct TestClient {
    replies: BTreeMap<String, Reply>,
    submissions: RefCell<Vec<FlowSubmission>>,
    terminals: RefCell<BTreeMap<String, NodeResult>>,
}

impl TestClient {
    fn new(replies: BTreeMap<String, Reply>) -> Rc<Self> {
        Rc::new(Self {
            replies,
            submissions: RefCell::default(),
            terminals: RefCell::default(),
        })
    }

    fn labels(&self) -> Vec<String> {
        self.submissions
            .borrow()
            .iter()
            .filter_map(|submission| submission.orchestration.node_label.clone())
            .collect()
    }

    fn submission(&self, label: &str) -> FlowSubmission {
        self.submissions
            .borrow()
            .iter()
            .find(|submission| submission.orchestration.node_label.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("no submission labelled {label}; saw {:?}", self.labels()))
            .clone()
    }

    fn brief(&self, label: &str) -> Value {
        self.submission(label)
            .spec
            .brief
            .unwrap_or_else(|| panic!("submission {label} carried no brief"))
    }

    fn submitted(&self, label: &str) -> bool {
        self.submissions
            .borrow()
            .iter()
            .any(|submission| submission.orchestration.node_label.as_deref() == Some(label))
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
        let label = submission
            .orchestration
            .node_label
            .clone()
            .unwrap_or_default();
        let reply = self
            .replies
            .get(&label)
            .unwrap_or_else(|| panic!("spec-build submitted an unscripted node {label:?}"))
            .clone();
        let task_uuid = submission
            .task_uuid
            .clone()
            .unwrap_or_else(|| format!("task-{index}"));
        let terminal = NodeResult {
            task_uuid: task_uuid.clone(),
            task_ref: submission.orchestration.task_ref.clone(),
            verdict: reply.verdict,
            exit_code: Some(i32::from(!reply.verdict.is_pass())),
            stderr_excerpt: None,
            stderr_truncated: None,
            witness_seq: u64::try_from(index + 1).expect("test ordinal fits u64"),
            disposition: Disposition::Created,
            model: None,
            result: reply.result,
            gates: None,
            error: reply.error,
        };
        self.terminals
            .borrow_mut()
            .insert(task_uuid.clone(), terminal);
        self.submissions.borrow_mut().push(submission.clone());
        Box::pin(std::future::ready(Ok(Admission {
            schema_version: 1,
            disposition: Disposition::Created,
            task_uuid,
            task_ref: submission.orchestration.task_ref,
            payload_hash: submission.payload_hash,
            attempt: 1,
            terminal: None,
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

fn checkpoint_task() -> Value {
    json!({
        "id": GATE_ID,
        "kind": "checkpoint",
        "title": "The chapter gate",
        "argv": ["true"],
        "runtimeMaxSec": 900,
        "dependencies": [],
        "revision": DIGEST
    })
}

/// A single-repository campaign whose whole frontier is one chapter gate, with
/// a steward bound: the diagnosis role this campaign dispatches is a catalog
/// steward, exactly as the incident's campaign had it.
fn args() -> Value {
    json!({
        "campaign": "fixture",
        "campaignIdentity": "0198a62b-41ee-7000-8000-000000000571",
        "repository": "acme/spec",
        "repositories": {
            "acme/spec": {
                "checkout": "/srv/spec/checkout",
                "baseBranch": "main",
                "remote": "origin",
                "forge": "local"
            }
        },
        "issue": {"number": "7", "url": "local://acme/spec/issues/7"},
        "runId": "run-steward-timeout",
        "worklist": "/srv/spec/checkout/worklist.json",
        "maxTasks": 8,
        "maxParallel": 1,
        "steering": [],
        "taskSteering": {},
        "localActor": "uid:1000",
        "steeringSource": {
            "schemaVersion": 1,
            "kind": "local-jsonl",
            "registrationId": "0198a62b-41ee-7000-8000-000000000571",
            "localActor": "uid:1000",
            "logPath": "/srv/spec/state/campaigns/steering/0198a62b-41ee-7000-8000-000000000571/steering-v1.jsonl",
            "lockPath": "/srv/spec/state/campaigns/steering/0198a62b-41ee-7000-8000-000000000571/steering.lock",
            "preparedCursor": 0
        },
        "agent": {
            "adapter": "codex",
            "argv": ["read the brief"],
            "priority": "low",
            "runtimeMaxSec": 14_400,
            "approvalPolicy": "never",
            "sandboxPolicy": "danger-full-access",
            "diagnosisSandboxPolicy": null,
            "model": null
        },
        "steward": {
            "adapter": "narrator",
            "argv": ["narrate", "--json"],
            "env": {},
            "finalMessagePattern": "^TALLY_FINAL_MESSAGE=(.*)$",
            "runtimeMaxSec": DEFAULT_STEWARD_DIAGNOSIS_RUNTIME_MAX_SEC
        },
        "gates": [
            {"kind": "forbidPaths", "id": "no-db", "forbidPaths": ["*.db"], "runtimeMaxSec": 900}
        ],
        "continuation": {
            "argv": ["/nix/store/tally/bin/tally", "campaign", "poll", "--once"],
            "pool": ["campaign-control"],
            "priority": "low",
            "eventsDir": "/srv/spec/events"
        },
        "workspaceRoot": "/srv/spec/worktrees",
        "captureRoot": "/srv/spec/state/capture/archive",
        "tally": "/nix/store/tally/bin/tally",
        "driver": "/nix/store/driver/bin/spec-build-driver",
        "driverRuntimeMaxSec": 900
    })
}

fn reconcile_result() -> Value {
    json!({
        "schemaVersion": 1,
        "campaign": "fixture",
        "repository": "acme/spec",
        "source": {"path": "worklist.json", "sha256": DIGEST, "revision": REV},
        "baseRevision": REV,
        "tasks": [checkpoint_task()],
        "merged": [],
        "checkpoints": [],
        "remaining": [GATE_ID],
        "frontier": [checkpoint_task()],
        "diagnoses": [],
        "retries": [],
        "deferrals": [],
        "blocked": [],
        "quiescent": false,
        "escalation": null,
        "complete": false,
        "warnings": [],
        "closingSummary": null
    })
}

/// Every node of the red-gate lane except the diagnosis itself, which each
/// case scripts for its own death.
fn base_replies() -> BTreeMap<String, Reply> {
    let mut replies = BTreeMap::new();
    replies.insert(
        "spec-build-sweep".to_owned(),
        Reply::passed(json!({
            "currentRunHash": "0123456789ab",
            "blockingJobs": [],
            "cleaned": [],
            "liveRuns": [],
            "warnings": []
        })),
    );
    replies.insert(
        "spec-build-reconcile".to_owned(),
        Reply::passed(reconcile_result()),
    );
    replies.insert(
        "spec-build-continue".to_owned(),
        Reply::passed(json!({
            "event": "/srv/spec/events/continuation.json",
            "dedupKey": "campaign:acme/spec:7:run-steward-timeout",
            "runId": "continuation-run-steward-timeout",
            "created": true,
            "receipt": null
        })),
    );
    replies.insert(
        format!("cleanup-{GATE_ID}"),
        Reply::passed(json!({"taskId": GATE_ID, "cleaned": true})),
    );
    replies.insert(
        format!("prep-{GATE_ID}"),
        Reply::passed(json!({
            "taskId": GATE_ID,
            "baseRev": PROVEN,
            "branch": "tally-work/fixture/chapter-gate",
            "publishBranch": "tally/spec-build/v1/fixture/chapter-gate",
            "worktreePath": "/srv/spec/worktrees/chapter-gate"
        })),
    );
    replies.insert(
        format!("checkpoint-{GATE_ID}"),
        Reply::projected_nothing("gate-failed", "the chapter gate is red"),
    );
    replies.insert(
        format!("checkpoint-record-{GATE_ID}"),
        Reply::passed(json!({
            "taskId": GATE_ID,
            "passed": false,
            "ref": null,
            "revision": PROVEN,
            "capturePath": "/srv/spec/state/capture/archive/chapter-gate.json",
            "stdoutTruncated": false,
            "stderrTruncated": false
        })),
    );
    replies.insert(
        format!("diff-{GATE_ID}"),
        Reply::passed(json!({
            "taskId": GATE_ID,
            "available": false,
            "baseRev": PROVEN,
            "head": null,
            "status": "",
            "patch": "",
            "truncated": false,
            "reason": "a checkpoint lane commits nothing"
        })),
    );
    replies.insert(
        format!("steer-{GATE_ID}"),
        Reply::passed(json!({
            "kind": "diagnosis",
            "taskId": GATE_ID,
            "attempt": 1,
            "comment": "local://acme/spec/diagnosis/chapter-gate/1",
            "verdict": "blocked",
            "blocked": true,
            "posted": true,
            "redacted": false
        })),
    );
    replies
}

fn run(client: Rc<TestClient>) -> Result<tally_flow::RunReport, Box<tally_flow::FlowError>> {
    let mut options = RunOptions::new("steward-timeout", args());
    options.max_nodes = 51;
    run_script(
        SOURCE,
        Some(Path::new("examples/flows/spec-build.js")),
        client,
        Rc::new(VecLifecycleSink::default()),
        options,
    )
    .map_err(Box::new)
}

/// Boa's schema walk over the campaign grammar needs more than libtest's
/// default worker stack.
fn on_flow_test_stack(test: fn()) {
    let outcome = std::thread::Builder::new()
        .name("steward-timeout".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(test)
        .expect("spawn the flow test with a bounded stack")
        .join();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// The incident's exact shape: the steward is killed at its unit budget and
/// projects no `finalMessage`, so the engine has no structured result to
/// validate. The lane must reach `steer` carrying the typed death, the pass
/// must end `blocked` rather than thrown, and the frontier's remaining
/// machinery — the continuation and the worktree cleanup — must still run.
fn a_projection_less_steward_death_case() {
    let mut replies = base_replies();
    replies.insert(
        format!("diagnose-{GATE_ID}"),
        Reply::projected_nothing(
            "result-schema-mismatch",
            "node returned no structured result",
        ),
    );
    let client = TestClient::new(replies);
    let report = run(client.clone()).expect("a dead judge must not end the pass");

    let steer = client.brief(&format!("steer-{GATE_ID}"));
    assert_eq!(
        steer["diagnosisUnavailable"]["code"],
        json!("result-schema-mismatch"),
        "the steer brief carries the typed death, not a judgment: {steer}"
    );
    assert!(
        steer["diagnosisUnavailable"]["detail"]
            .as_str()
            .is_some_and(|detail| !detail.is_empty()),
        "the dead node's own detail travels into the receipt: {steer}"
    );
    assert!(
        steer.get("diagnosis").is_none(),
        "a judgment nobody made must not be claimed as one: {steer}"
    );
    assert_eq!(steer["verdict"], json!("blocked"), "{steer}");

    // The rest of the pass ran. This is the half the incident cost: the
    // continuation that keeps the campaign moving and the cleanup that returns
    // the worktree both come after the diagnosis lane.
    assert!(
        client.submitted("spec-build-continue"),
        "the pass must still write its continuation; submissions were {:?}",
        client.labels()
    );
    assert!(
        client.submitted(&format!("cleanup-{GATE_ID}")),
        "the pass must still clean its lane; submissions were {:?}",
        client.labels()
    );

    let value = report.final_value.as_ref().expect("the pass returned");
    assert_eq!(
        value["state"],
        json!("blocked"),
        "no diagnosis means stop and tell the operator: {value}"
    );
    let diagnoses = value["diagnoses"]
        .as_array()
        .expect("diagnoses is an array");
    assert_eq!(diagnoses.len(), 1, "{value}");
    assert_eq!(diagnoses[0]["blocked"], json!(true), "{value}");
    assert_eq!(value["published"], json!(null), "a red gate proves nothing");
}

/// A steward that exits zero with an answer the diagnosis schema rejects is
/// the same outcome as one killed at its budget: nothing was judged. This node
/// keeps a passing verdict and its rejected value stays attached, so reading
/// "passed" alone would hand the deterministic rails a verdict no judge ever
/// returned — and the steer node would then die on it, ending the pass by the
/// other door.
fn an_unusable_steward_answer_case() {
    let mut replies = base_replies();
    replies.insert(
        format!("diagnose-{GATE_ID}"),
        Reply::passed(json!({"verdict": "maybe"})),
    );
    let client = TestClient::new(replies);
    let report = run(client.clone()).expect("an unusable answer must not end the pass");

    let steer = client.brief(&format!("steer-{GATE_ID}"));
    assert!(
        steer.get("diagnosis").is_none(),
        "a rejected answer is not a judgment: {steer}"
    );
    assert_eq!(
        steer["diagnosisUnavailable"]["code"],
        json!("result-schema-mismatch"),
        "{steer}"
    );
    let value = report.final_value.as_ref().expect("the pass returned");
    assert_eq!(value["state"], json!("blocked"), "{value}");
}

/// The budget the incident blamed, bound where it is spent: the steward role's
/// runtime reaches the diagnosis node itself. A campaign that states no
/// steward budget gets `DEFAULT_STEWARD_DIAGNOSIS_RUNTIME_MAX_SEC`, which is
/// the number this fixture carries.
fn the_diagnosis_node_carries_the_ruled_budget_case() {
    let mut replies = base_replies();
    replies.insert(
        format!("diagnose-{GATE_ID}"),
        Reply::passed(json!({
            "verdict": "blocked",
            "diagnosis": "Failed the chapter gate on the accumulated tree."
        })),
    );
    let client = TestClient::new(replies);
    run(client.clone()).expect("the pass completes and steers");

    let diagnosis = client.submission(&format!("diagnose-{GATE_ID}"));
    assert_eq!(
        diagnosis.spec.runtime_max_sec,
        Some(DEFAULT_STEWARD_DIAGNOSIS_RUNTIME_MAX_SEC),
        "the steward role's ruled budget is what the diagnosis node runs under"
    );
    const {
        assert!(
            DEFAULT_STEWARD_DIAGNOSIS_RUNTIME_MAX_SEC > 120,
            "the 120-second budget the eta C1 re-witness killed a pass on is retired"
        );
    }
}

#[test]
fn steward_timeout_death_is_a_blocked_escalation_not_a_pass_failure() {
    on_flow_test_stack(a_projection_less_steward_death_case);
}

#[test]
fn steward_timeout_unusable_answer_reaches_the_operator_too() {
    on_flow_test_stack(an_unusable_steward_answer_case);
}

#[test]
fn steward_timeout_budget_is_the_ruled_diagnosis_role_budget() {
    on_flow_test_stack(the_diagnosis_node_carries_the_ruled_budget_case);
}
