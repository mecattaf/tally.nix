//! #424 ruling 2, bound at the flow: a pass whose agent node fails still runs
//! the tree-delta permission gate before the pass ends.
//!
//! The driver half of that ruling is covered thoroughly in Python
//! (`action_tree_delta` with `ownershipRan: false`, the declared allowlist, the
//! refusal). None of it proves the flow ever *calls* the gate on a failed pass.
//! The round-1 eval deleted the whole `strayDelta` block from `spec-build.js`
//! — restoring the exact pre-#424 "never runs treeDelta (no call made)"
//! behaviour the issue describes — and every suite in the workspace stayed
//! green. This file is the binding: it drives the real `spec-build.js` end to
//! end against a scripted client, fails the agent node, and reads the
//! submissions the flow actually made.

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
const TASK_ID: &str = "build";
const REV: &str = "0123456789abcdef0123456789abcdef01234567";
const RESTAMP_HEAD: &str = "1111111111111111111111111111111111111111";
const DIGEST: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OLD_DIGEST: &str = "sha256:1111111111111111111111111111111111111111111111111111111111111111";

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

    /// A node the daemon witnessed as failed. Every node this fixture fails is
    /// dispatched with `settle: true`, so the flow sees it as data.
    fn failed(code: &str, message: &str) -> Self {
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

/// Replies keyed by node label rather than by ordinal, so a fixture cannot
/// silently answer the wrong node when the flow's node order shifts.
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

/// A frontier task shaped like `implementationTaskSchema`: the arm a file-based
/// worklist produces.
fn implementation_task(conflict_domains: Option<Value>) -> Value {
    let mut task = json!({
        "id": TASK_ID,
        "kind": "implementation",
        "title": "Build the thing",
        "goal": "Deliver the bounded behaviour.",
        "deliveredBehaviors": ["the thing exists"],
        "readFirst": {"specSections": ["spec.md#build"], "styleReferences": []},
        "acceptanceCriteria": [
            {"id": "focused", "description": "The focused check passes.", "argv": ["true"]}
        ],
        "dependencies": [],
        "revision": DIGEST
    });
    if let Some(domains) = conflict_domains {
        task["conflictDomains"] = domains;
    }
    task
}

/// A frontier task shaped like `issueTaskSchema`: the arm the forge-native
/// issue-graph builder produces, and therefore the arm every ad-hoc campaign in
/// production actually runs on. `taskSchema` is one `oneOf` over both arms and
/// does not depend on the campaign mode, so a fixture may exercise this arm
/// through the same run as the other one; what is being pinned is the schema,
/// not the producer.
fn issue_task(conflict_domains: Option<Value>) -> Value {
    let mut task = json!({
        "id": TASK_ID,
        "kind": "implementation",
        "title": "Build the thing",
        "brief": {
            "issue": {"number": "8", "url": "https://github.test/acme/spec/issues/8"},
            "body": "Deliver the bounded behaviour."
        },
        "dependencies": [],
        "revision": DIGEST
    });
    if let Some(domains) = conflict_domains {
        task["conflictDomains"] = domains;
    }
    task
}

/// A single-repository campaign with one implementation task and one
/// `forbidPaths` gate. No command gate, so the pristine-base preflight lane is
/// not admitted and the pass is the shortest one that reaches an agent node.
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
        "runId": "run-424",
        "worklist": "/srv/spec/checkout/worklist.json",
        "maxTasks": 8,
        "maxParallel": 1,
        "steering": [],
        "taskSteering": {},
        "allowedActors": ["operator"],
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
        "driver": "/nix/store/driver/spec_build_driver.py",
        "driverRuntimeMaxSec": 900
    })
}

fn reconcile_result(task: Value) -> Value {
    json!({
        "schemaVersion": 1,
        "campaign": "fixture",
        "repository": "acme/spec",
        "source": {"path": "worklist.json", "sha256": DIGEST, "revision": REV},
        "baseRevision": REV,
        "tasks": [task.clone()],
        "merged": [],
        "restamps": [],
        "checkpoints": [],
        "remaining": [TASK_ID],
        "frontier": [task],
        "diagnoses": [],
        "retries": [],
        "deferrals": [],
        "blocked": [],
        "quiescent": false,
        "escalation": null,
        "complete": false,
        "anomalies": [],
        "warnings": [],
        "closingSummary": null
    })
}

/// Every node of a pass whose agent fails, with `tree_delta` scripted by the
/// caller. Nothing here is ordinal-sensitive: the client answers by label.
fn replies(task: Value, tree_delta: Reply) -> BTreeMap<String, Reply> {
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
        Reply::passed(reconcile_result(task)),
    );
    replies.insert(
        format!("prep-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "baseRev": REV,
            "branch": "tally-work/fixture/build",
            "publishBranch": "tally/spec-build/v1/fixture/build",
            "worktreePath": "/srv/spec/worktrees/build"
        })),
    );
    replies.insert(
        format!("steering-recheck-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "authorizedComments": [],
            "receipt": {
                "source": {
                    "kind": "local-jsonl",
                    "registrationId": "0198a62b-41ee-7000-8000-000000000571",
                    "path": "/srv/spec/state/campaigns/steering/0198a62b-41ee-7000-8000-000000000571/steering-v1.jsonl",
                    "preparedCursor": 0,
                    "recheckedCursor": 0
                },
                "rechecked": true,
                "recheckTruncated": false,
                "preparedCommentIds": [],
                "lateRecheckCommentIds": []
            }
        })),
    );
    // The whole premise: the agent node did not pass.
    replies.insert(
        format!("agent-{TASK_ID}"),
        Reply::failed("worker-failed", "the agent exited non-zero"),
    );
    replies.insert(format!("tree-delta-{TASK_ID}"), tree_delta);
    replies.insert(
        format!("diff-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "available": true,
            "baseRev": REV,
            "head": REV,
            "status": "M internal/cli/root.go",
            "patch": "",
            "truncated": false,
            "reason": null
        })),
    );
    replies.insert(
        format!("diagnose-{TASK_ID}"),
        Reply::passed(json!("The attempt failed before it committed anything.")),
    );
    replies.insert(
        format!("steer-{TASK_ID}"),
        Reply::passed(json!({
            "kind": "diagnosis",
            "taskId": TASK_ID,
            "attempt": 2,
            "comment": "local://acme/spec/diagnosis/build/2",
            "blocked": true,
            "posted": true,
            "redacted": false
        })),
    );
    replies.insert(
        "spec-build-continue".to_owned(),
        Reply::passed(json!({
            "event": "/srv/spec/events/continuation.json",
            "dedupKey": "campaign:acme/spec:7:run-424",
            "runId": "continuation-run-424",
            "created": true,
            "receipt": null
        })),
    );
    replies.insert(
        format!("cleanup-{TASK_ID}"),
        Reply::passed(json!({"taskId": TASK_ID, "cleaned": true})),
    );
    replies
}

/// One successful deterministic re-stamp lane. An unscripted submission is a
/// test panic, so deliberately omitting every agent label is the executable
/// assertion that the flow never tries to dispatch one.
fn restamp_replies(task: Value) -> BTreeMap<String, Reply> {
    let completion = json!({
        "taskId": TASK_ID,
        "pullRequest": "https://github.com/acme/spec/pull/8",
        "mergeCommit": REV,
        "revision": OLD_DIGEST
    });
    let ownership = json!({
        "taskId": TASK_ID,
        "domainsRequired": false,
        "ownedPaths": [],
        "baseRev": REV,
        "head": RESTAMP_HEAD
    });
    let narration = json!({
        "source": "template",
        "subject": "chore(campaign): re-stamp completion",
        "body": ""
    });
    let mut reconciled = reconcile_result(task.clone());
    reconciled["restamps"] = json!([completion.clone()]);

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
    replies.insert("spec-build-reconcile".to_owned(), Reply::passed(reconciled));
    replies.insert(
        format!("prep-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "baseRev": REV,
            "branch": "tally-work/fixture/build",
            "publishBranch": "tally/fixture-issue-7/build-0123456789abcdef",
            "worktreePath": "/srv/spec/worktrees/build"
        })),
    );
    replies.insert(
        format!("restamp-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "head": RESTAMP_HEAD,
            "revision": DIGEST,
            "completion": completion
        })),
    );
    replies.insert(
        format!("ownership-{TASK_ID}"),
        Reply::passed(ownership.clone()),
    );
    replies.insert(
        format!("tree-delta-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "checkedPaths": 0,
            "allowlistBasis": "owned-paths-fallback",
            "allowlist": [],
            "ownershipRan": true
        })),
    );
    replies.insert(
        format!("gate-{TASK_ID}-no-db"),
        Reply::passed(json!({
            "gateId": "no-db",
            "kind": "forbidPaths",
            "patterns": ["*.db"],
            "checkedPaths": 0,
            "baseRev": REV,
            "head": RESTAMP_HEAD
        })),
    );
    replies.insert(
        format!("publish-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "branch": "tally/fixture-issue-7/build-0123456789abcdef",
            "head": RESTAMP_HEAD,
            "pullRequest": "https://github.com/acme/spec/pull/9",
            "narration": narration.clone(),
            "narrationAttempts": [],
            "ownership": ownership.clone()
        })),
    );
    replies.insert(
        format!("rebase-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "baseRev": REV,
            "branch": "tally/fixture-issue-7/build-0123456789abcdef",
            "head": RESTAMP_HEAD,
            "pullRequest": "https://github.com/acme/spec/pull/9",
            "narration": narration,
            "regate": false,
            "ownership": ownership.clone()
        })),
    );
    replies.insert(
        format!("merge-{TASK_ID}"),
        Reply::passed(json!({
            "taskId": TASK_ID,
            "head": RESTAMP_HEAD,
            "mergeCommit": "2222222222222222222222222222222222222222",
            "pullRequest": "https://github.com/acme/spec/pull/9",
            "regated": false,
            "ownership": ownership,
            "trailer": null
        })),
    );
    replies.insert(
        "spec-build-continue".to_owned(),
        Reply::passed(json!({
            "event": "/srv/spec/events/continuation.json",
            "dedupKey": "campaign:acme/spec:7:run-424",
            "runId": "continuation-run-424",
            "created": true,
            "receipt": null
        })),
    );
    replies.insert(
        format!("cleanup-{TASK_ID}"),
        Reply::passed(json!({"taskId": TASK_ID, "cleaned": true})),
    );
    replies
}

fn run(client: Rc<TestClient>) -> Result<tally_flow::RunReport, Box<tally_flow::FlowError>> {
    let mut options = RunOptions::new("spec-build-424", args());
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

// Boa's unoptimized schema walk now includes the complete canonical campaign
// grammar and needs more than libtest's 2 MiB worker stack. Production and the
// flake's release tests already have ample headroom; keep debug runs useful too.
fn on_flow_test_stack(test: fn()) {
    let outcome = std::thread::Builder::new()
        .name("spec-build-failed-agent-gate".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(test)
        .expect("spawn the flow test with a bounded stack")
        .join();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

/// #424 ruling 2. The agent node fails; the pass must still dispatch the
/// tree-delta gate before it ends, and must dispatch it with
/// `ownershipRan: false` — ownership never ran, so the driver has no certified
/// `ownedPaths` to fall back to and only a declared allowlist may govern.
///
/// Deleting the `strayDelta` block from `spec-build.js` (the eval's M8:
/// restoring the pre-#424 return at stage "agent") makes this red on the
/// `tree-delta-build` submission that is then never made.
fn a_failed_agent_pass_still_dispatches_the_tree_delta_gate_case() {
    let task = implementation_task(Some(json!(["README.md"])));
    // The gate finds the stray write the failing agent left.
    let client = TestClient::new(replies(
        task,
        Reply::failed(
            "driver-failed",
            "tree-delta gate detected 1 out-of-allowlist change(s) (declared allowlist)",
        ),
    ));
    let report = run(client.clone()).expect("the pass completes and steers");

    assert!(
        client.submitted(&format!("tree-delta-{TASK_ID}")),
        "a pass whose agent node failed must still run the tree-delta gate; \
         submissions were {:?}",
        client.labels()
    );
    // Dispatched before the pass ends, and after the agent node it is judging.
    let labels = client.labels();
    let agent = labels
        .iter()
        .position(|label| label == &format!("agent-{TASK_ID}"))
        .expect("the agent node ran");
    let gate = labels
        .iter()
        .position(|label| label == &format!("tree-delta-{TASK_ID}"))
        .expect("the gate ran");
    assert!(agent < gate, "{labels:?}");

    let brief = client.brief(&format!("tree-delta-{TASK_ID}"));
    assert_eq!(
        brief["ownershipRan"],
        json!(false),
        "the gate must be told ownership never ran, or it would fall back to \
         `ownedPaths` that were never certified: {brief}"
    );
    assert_eq!(brief["task"]["id"], json!(TASK_ID));
    assert_eq!(
        brief["workspace"]["worktreePath"],
        json!("/srv/spec/worktrees/build")
    );
    assert!(
        brief.get("ownedPaths").is_none(),
        "no ownership node ran, so there is no owned-path set to send: {brief}"
    );

    // The gate's verdict is what the pass reports, not the agent's: a lane that
    // breached is aborted rather than steered for another attempt.
    let steer = client.brief(&format!("steer-{TASK_ID}"));
    assert_eq!(steer["breach"], json!(true), "{steer}");
    assert_eq!(report.final_value.as_ref().unwrap()["state"], "steered");
}

/// The other outcome of the same call: the gate passes, so the pass reports the
/// agent failure it always reported. The new node must not swallow the failure
/// that caused it to run.
fn a_failed_agent_pass_whose_gate_passes_still_reports_the_agent_failure_case() {
    let task = implementation_task(Some(json!(["README.md"])));
    let client = TestClient::new(replies(
        task,
        Reply::passed(json!({
            "taskId": TASK_ID,
            "checkedPaths": 0,
            "allowlistBasis": "declared",
            "allowlist": ["README.md"],
            "ownershipRan": false
        })),
    ));
    run(client.clone()).expect("the pass completes and steers");

    assert!(client.submitted(&format!("tree-delta-{TASK_ID}")));
    let steer = client.brief(&format!("steer-{TASK_ID}"));
    assert!(
        steer.get("breach").is_none(),
        "a clean gate must leave the agent failure priced as work: {steer}"
    );
    assert_eq!(steer["attempt"], json!(1), "{steer}");
}

/// One arm of the pin below: a serial frontier carrying `task` must preserve
/// the omitted key through the live task, agent brief, and failed-agent gate.
fn assert_keyless_task_reaches_ungated_gate(arm: &str, task: Value) {
    let client = TestClient::new(replies(
        task,
        Reply::failed(
            "driver-failed",
            "tree-delta gate refuses to judge: ownership never ran and the task declares no conflictDomains",
        ),
    ));
    let report = run(client.clone()).unwrap_or_else(|error| {
        panic!("the {arm} frontier task must reach its fail-closed gate: {error:?}")
    });

    assert!(
        client.submitted(&format!("prep-{TASK_ID}")),
        "{arm} arm: the admitted serial task must start a lane; submissions were {:?}",
        client.labels()
    );
    let agent = client.brief(&format!("agent-{TASK_ID}"));
    assert!(
        agent["task"].get("conflictDomains").is_none(),
        "{arm} arm: the implementation brief must preserve omission: {agent}"
    );
    let gate = client.brief(&format!("tree-delta-{TASK_ID}"));
    assert!(
        gate["task"].get("conflictDomains").is_none(),
        "{arm} arm: the failed-agent gate must receive omission, not []: {gate}"
    );
    assert_eq!(gate["ownershipRan"], json!(false), "{arm} arm: {gate}");
    let diagnosis = client.brief(&format!("diagnose-{TASK_ID}"));
    assert!(
        diagnosis["task"].get("conflictDomains").is_none(),
        "{arm} arm: retry/diagnosis payloads must preserve omission: {diagnosis}"
    );
    let steer = client.brief(&format!("steer-{TASK_ID}"));
    assert_eq!(steer["breach"], json!(true), "{arm} arm: {steer}");
    assert_eq!(
        steer["abortReason"],
        json!("tree-delta-ungated"),
        "{arm} arm: {steer}"
    );
    assert_eq!(
        report.final_value.as_ref().unwrap()["state"],
        "steered",
        "{arm} arm"
    );
}

/// The optional wire shape is pinned on both implementation arms of
/// `taskSchema`.
///
/// The refusal branch of `action_tree_delta` — #424 ruling 3, "no allowlist, no
/// pass" — is the required failed-agent outcome when an admitted serial task
/// declares no `conflictDomains`: no ownership receipt exists to supply the
/// passing path's owned-path fallback. The flow must accept that task and keep
/// the key absent all the way to the gate; inserting `[]` would turn an
/// unjudgeable pass into a false declared-empty breach.
///
/// `taskSchema` is a `oneOf` over four arms and two of them are implementation
/// arms: `implementationTaskSchema`, which a file-based worklist produces, and
/// `issueTaskSchema`, which the forge-native issue-graph builder produces and
/// which every ad-hoc campaign in production therefore runs on. Round 2 of the
/// eval found that relaxing `required` on the second arm alone left every suite
/// green, so both are exercised here. The `oneOf` does not depend on the
/// campaign mode, so one run shape reaches both arms; what is pinned is the
/// schema, not the producer. (The two checkpoint arms do not carry the key and
/// do not need to: a checkpoint lane returns before the agent node and can
/// reach neither treeDelta call.)
///
/// If either arm makes the field required again or a composition step inserts
/// `[]`, this test fails at the exact boundary that lost the third state.
fn both_implementation_arms_preserve_an_omitted_conflict_domain_case() {
    assert_keyless_task_reaches_ungated_gate(
        "file-based implementationTaskSchema",
        implementation_task(None),
    );
    assert_keyless_task_reaches_ungated_gate("forge-native issueTaskSchema", issue_task(None));
}

/// #459 tier 2. A historical fact admitted by the completion oracle takes the
/// deterministic restamp node and rejoins the ordinary proof pipeline at
/// ownership. It never submits steering, implementation, diagnosis, or
/// narration agents.
fn a_restamp_lane_never_dispatches_an_agent_case() {
    let client = TestClient::new(restamp_replies(issue_task(None)));
    let report = run(client.clone()).expect("the deterministic marker lane completes");
    let labels = client.labels();
    assert!(
        client.submitted(&format!("restamp-{TASK_ID}")),
        "{labels:?}"
    );
    assert!(
        client.submitted(&format!("ownership-{TASK_ID}")),
        "{labels:?}"
    );
    assert!(client.submitted(&format!("merge-{TASK_ID}")), "{labels:?}");
    for forbidden in [
        format!("steering-recheck-{TASK_ID}"),
        format!("agent-{TASK_ID}"),
        format!("diagnose-{TASK_ID}"),
        format!("steer-{TASK_ID}"),
    ] {
        assert!(
            !client.submitted(&forbidden),
            "an agent-free restamp submitted {forbidden}: {labels:?}"
        );
    }
    let publish = client.brief(&format!("publish-{TASK_ID}"));
    assert_eq!(
        publish["steward"],
        Value::Null,
        "the publish node must use deterministic narration: {publish}"
    );
    let merge = client.brief(&format!("merge-{TASK_ID}"));
    assert_eq!(merge["assistedBy"], Value::Null, "{merge}");
    assert_eq!(report.final_value.as_ref().unwrap()["state"], "advanced");
}

#[test]
fn a_failed_agent_pass_still_dispatches_the_tree_delta_gate() {
    on_flow_test_stack(a_failed_agent_pass_still_dispatches_the_tree_delta_gate_case);
}

#[test]
fn a_failed_agent_pass_whose_gate_passes_still_reports_the_agent_failure() {
    on_flow_test_stack(a_failed_agent_pass_whose_gate_passes_still_reports_the_agent_failure_case);
}

#[test]
fn both_implementation_arms_preserve_an_omitted_conflict_domain() {
    on_flow_test_stack(both_implementation_arms_preserve_an_omitted_conflict_domain_case);
}

#[test]
fn a_restamp_lane_never_dispatches_an_agent() {
    on_flow_test_stack(a_restamp_lane_never_dispatches_an_agent_case);
}
