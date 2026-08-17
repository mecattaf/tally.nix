//! The single-line integration model, bound at the flow: a pass that carries a
//! durable gate proof dispatches the publish node, and a pass that carries none
//! never does.
//!
//! The driver half — which head fast-forwards `main`, which one is refused, and
//! what the receipt records — is proven against real repositories in
//! `spec-build-driver`. None of that proves the flow ever *calls* the node.
//! Delete the `publishProvenHead` call from `spec-build.js` and every driver
//! test stays green while `main` never moves again; this file is the binding.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use serde_json::{json, Value};
use tally_flow::{
    run_script, Admission, ClientError, Disposition, FlowClient, FlowFuture, FlowSubmission,
    NodeFailure, NodeResult, RunInspection, RunOptions, VecLifecycleSink, Verdict,
};

const SOURCE: &str = include_str!("../../../examples/flows/spec-build.js");
const GATE_ID: &str = "chapter-gate";
const REV: &str = "0123456789abcdef0123456789abcdef01234567";
const PROVEN: &str = "fedcba9876543210fedcba9876543210fedcba98";
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const PROOF_REF: &str = "refs/tally/spec-build/v1/fixture-7/checkpoint/chapter-gate-0123/\
fedcba9876543210fedcba9876543210fedcba98";

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

    fn brief(&self, label: &str) -> Value {
        self.submissions
            .borrow()
            .iter()
            .find(|submission| submission.orchestration.node_label.as_deref() == Some(label))
            .unwrap_or_else(|| panic!("no submission labelled {label}; saw {:?}", self.labels()))
            .spec
            .brief
            .clone()
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

/// A single-repository campaign whose whole frontier is one chapter gate. No
/// command gate, so no pristine-base preflight lane is admitted.
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
        "runId": "run-publish",
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

fn reconcile_result(checkpoints: Value, frontier: Value, remaining: Value) -> Value {
    json!({
        "schemaVersion": 1,
        "campaign": "fixture",
        "repository": "acme/spec",
        "source": {"path": "worklist.json", "sha256": DIGEST, "revision": REV},
        "baseRevision": REV,
        "tasks": [checkpoint_task()],
        "merged": [],
        "checkpoints": checkpoints,
        "remaining": remaining,
        "frontier": frontier,
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

fn publish_result(action: &str, sha: Value, receipt: Value) -> Value {
    json!({
        "action": action,
        "baseBranch": "main",
        "sha": sha,
        "integrationHead": PROVEN,
        "provenHead": PROVEN,
        "regateRequired": false,
        "receipt": receipt,
        "receiptRef": format!("refs/tally/spec-build/v1/fixture-7/publish/{PROVEN}"),
        "reason": "main fast-forwards to the gate-proven head"
    })
}

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
        "spec-build-continue".to_owned(),
        Reply::passed(json!({
            "event": "/srv/spec/events/continuation.json",
            "dedupKey": "campaign:acme/spec:7:run-publish",
            "runId": "continuation-run-publish",
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
        Reply::passed(json!({"ok": true})),
    );
    replies.insert(
        format!("checkpoint-record-{GATE_ID}"),
        Reply::passed(json!({
            "taskId": GATE_ID,
            "passed": true,
            "ref": PROOF_REF,
            "revision": PROVEN,
            "capturePath": "/srv/spec/state/capture/archive/chapter-gate.json",
            "stdoutTruncated": false,
            "stderrTruncated": false
        })),
    );
    replies
}

fn run(client: Rc<TestClient>) -> Result<tally_flow::RunReport, Box<tally_flow::FlowError>> {
    let mut options = RunOptions::new("spec-build-publish", args());
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
        .name("spec-build-publish".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(test)
        .expect("spawn the flow test with a bounded stack")
        .join();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn a_settled_gate_publishes_its_own_proven_head_case() {
    let mut replies = base_replies();
    replies.insert(
        "spec-build-reconcile".to_owned(),
        Reply::passed(reconcile_result(
            json!([]),
            json!([checkpoint_task()]),
            json!([GATE_ID]),
        )),
    );
    replies.insert(
        format!("publish-main-{GATE_ID}"),
        Reply::passed(publish_result(
            "fast-forward",
            json!(PROVEN),
            json!({
                "schemaVersion": 1,
                "campaign": "fixture",
                "baseBranch": "main",
                "sha": PROVEN,
                "provenBy": {"taskId": GATE_ID, "reference": PROOF_REF},
                "actor": "spec-build-driver",
                "writtenAt": "2026-08-17T12:00:00.000Z"
            }),
        )),
    );
    let client = TestClient::new(replies);
    let report = run(client.clone()).expect("the pass completes and publishes");

    let label = format!("publish-main-{GATE_ID}");
    assert!(
        client.submitted(&label),
        "a pass whose chapter gate settled must offer its proven head to main; \
         submissions were {:?}",
        client.labels()
    );
    // Dispatched after the gate's own receipt exists: the proof is a durable
    // fact before it is offered as one.
    let labels = client.labels();
    let recorded = labels
        .iter()
        .position(|entry| entry == &format!("checkpoint-record-{GATE_ID}"))
        .expect("the checkpoint receipt was written");
    let published = labels
        .iter()
        .position(|entry| entry == &label)
        .expect("the publish node ran");
    assert!(recorded < published, "{labels:?}");

    let brief = client.brief(&label);
    assert_eq!(
        brief["proofs"],
        json!([{"taskId": GATE_ID, "ref": PROOF_REF, "revision": PROVEN}]),
        "the publish node reads the gate's durable proof: {brief}"
    );
    assert_eq!(brief["source"]["sha256"], json!(DIGEST), "{brief}");

    let value = report.final_value.as_ref().expect("the pass returned");
    assert_eq!(value["published"]["sha"], json!(PROVEN));
    assert_eq!(value["published"]["receipt"]["sha"], json!(PROVEN));
    // One sha on the record: the receipt names the revision, and the proof it
    // names is the proof of that same revision.
    assert_eq!(
        value["published"]["receipt"]["provenBy"]["reference"],
        json!(PROOF_REF)
    );
    assert!(value["published"]["receipt"]["provenBy"]["reference"]
        .as_str()
        .unwrap()
        .ends_with(PROVEN));
}

fn an_unproven_pass_publishes_nothing_to_main_case() {
    let mut replies = base_replies();
    // The gate is still deferred behind unrelated work: nothing has proven a
    // head this pass, and nothing proved one before it.
    replies.insert(
        "spec-build-reconcile".to_owned(),
        Reply::passed(reconcile_result(
            json!([]),
            json!([checkpoint_task()]),
            json!([GATE_ID]),
        )),
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
        format!("checkpoint-{GATE_ID}"),
        Reply {
            verdict: Verdict::Failed,
            result: None,
            error: Some(NodeFailure {
                code: "gate-failed".to_owned(),
                message: "the chapter gate is red".to_owned(),
                details: None,
            }),
        },
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
        format!("diagnose-{GATE_ID}"),
        Reply::passed(json!("Failed the chapter gate on the accumulated tree.")),
    );
    replies.insert(
        format!("steer-{GATE_ID}"),
        Reply::passed(json!({
            "kind": "diagnosis",
            "taskId": GATE_ID,
            "attempt": 1,
            "comment": "local://acme/spec/diagnosis/chapter-gate/1",
            "blocked": true,
            "posted": true,
            "redacted": false
        })),
    );
    let client = TestClient::new(replies);
    let report = run(client.clone()).expect("the pass completes and steers");

    assert!(
        !client.submitted(&format!("publish-main-{GATE_ID}")),
        "a red gate proves no head, so nothing may be offered to main; \
         submissions were {:?}",
        client.labels()
    );
    let value = report.final_value.as_ref().expect("the pass returned");
    assert_eq!(value["published"], json!(null));
}

#[test]
fn a_settled_gate_publishes_its_own_proven_head() {
    on_flow_test_stack(a_settled_gate_publishes_its_own_proven_head_case);
}

#[test]
fn an_unproven_pass_publishes_nothing_to_main() {
    on_flow_test_stack(an_unproven_pass_publishes_nothing_to_main_case);
}
