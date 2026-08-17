//! Executable tests for the pure decision helpers in `examples/flows/spec-build.js`.
//!
//! The campaign flow carries real branching logic — how a lane failure is
//! priced, which steering comments reach which agent — and until now none of it
//! had a test: the flake checks assert only that certain call shapes appear in
//! the source. A ripgrep match cannot tell a composed value from a dropped one.
//!
//! There is no JavaScript module system and there never will be (§6), so this
//! harness does not import anything. It evaluates the flow source in a bare Boa
//! realm exactly as a Script, which is what the engine does, and then calls the
//! top-level function declarations by name. The campaign body is one async IIFE:
//! it hoists every helper before it runs, it never throws synchronously, and
//! with no job executor driving microtasks it suspends at its first `await` and
//! stays there. Nothing this harness calls performs I/O or reaches a host API.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsValue, Script, Source};
use serde_json::{json, Value};

use crate::{check_script, CheckOptions};

fn spec_build_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/flows/spec-build.js")
}

fn local_campaign_args(runner_pool: &str) -> Value {
    let digest = format!("sha256:{}", "a".repeat(64));
    let manifest = json!({
        "schemaVersion": 1,
        "name": "fixture",
        "repository": {
            "checkout": "/tmp/tally-fixture",
            "baseBranch": "main",
            "remote": "origin",
            "forge": "local"
        },
        "maxTasks": 1,
        "maxParallel": 1,
        "driverRuntimeMaxSec": 60,
        "runtimeMaxSec": null,
        "pool": runner_pool,
        "mergeMethod": "squash",
        "agent": {
            "adapter": "codex",
            "argv": ["implement"],
            "priority": "medium",
            "runtimeMaxSec": null,
            "approvalPolicy": null,
            "sandboxPolicy": null,
            "diagnosisSandboxPolicy": null,
            "model": null
        },
        "steward": null,
        "gates": [{
            "kind": "forbidPaths",
            "id": "scope",
            "forbidPaths": ["flake.nix"],
            "runtimeMaxSec": 60
        }],
        "tasks": [{
            "id": "task-1",
            "kind": "implementation",
            "issue": 537,
            "dependencies": [],
            "argv": null,
            "runtimeMaxSec": null
        }]
    });
    json!({
        "campaignIdentity": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
        "campaignGraph": {
            "manifest": manifest.clone(),
            "tasks": [{
                "number": 537,
                "title": "Fixture task",
                "body": "Implement the fixture."
            }],
            "executableDigest": digest.clone()
        },
        "repository": "mecattaf/tally.nix",
        "issue": {
            "number": "1",
            "url": "local://mecattaf/tally.nix/specs/ch2.json"
        },
        "runId": "fixture-run",
        // campaign.rs still emits this opaque selector spelling. The flow
        // verifies its digest, then reads the committed local path above.
        "worklist": {
            "kind": "github-issue",
            "graphDigest": digest
        },
        "armedManifest": manifest,
        "continuation": {
            "argv": ["tally", "campaign", "resume"],
            "pool": ["campaign-control"],
            "priority": "low",
            "runtimeMaxSec": 60,
            "eventsDir": "/tmp/tally-state/events"
        },
        "workspaceRoot": "/tmp/tally-workspaces",
        "captureRoot": "/tmp/tally-state/capture/archive",
        "tally": "/bin/tally",
        "driver": "/bin/spec-build-driver",
        "driverRuntimeMaxSec": 60,
        "steering": [],
        "taskSteering": {},
        "localActor": "uid:1000",
        "steeringSource": {
            "schemaVersion": 1,
            "kind": "local-jsonl",
            "registrationId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
            "localActor": "uid:1000",
            "logPath": "/tmp/tally-state/steering.jsonl",
            "lockPath": "/tmp/tally-state/steering.lock",
            "preparedCursor": 0
        },
        // Accepted but ignored until campaign.rs drops these transport fields.
        "allowedActors": ["local"],
        "capabilities": {"subIssueWalk": false}
    })
}

fn check_campaign_args(args: &Value) -> Result<(), crate::FlowError> {
    let args = args.clone();
    thread::Builder::new()
        .name("campaign-flow-schema".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(move || {
            let path = spec_build_path();
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            check_script(
                &source,
                Some(&path),
                CheckOptions {
                    args: Some(&args),
                    ..CheckOptions::default()
                },
            )
            .map(|_| ())
        })
        .expect("campaign flow schema worker must start")
        .join()
        .expect("campaign flow schema worker must stop cleanly")
}

enum CampaignFlowRequest {
    Call {
        name: String,
        arguments: Vec<Value>,
        response: mpsc::SyncSender<Value>,
    },
}

/// One evaluated `spec-build.js` realm bound to a fixed `args`.
///
/// Boa recursively instantiates the declarations in this roughly 100 KiB
/// script. Keep its context on a dedicated, bounded large-stack test thread so
/// ordinary additions to the flow do not make Rust's small default test-thread
/// stack the accidental size limit for valid JavaScript.
struct CampaignFlowRealm {
    requests: Option<mpsc::Sender<CampaignFlowRequest>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl CampaignFlowRealm {
    fn new(args: &Value) -> Self {
        let args = args.clone();
        let (request_tx, request_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::sync_channel(0);
        let worker = thread::Builder::new()
            .name("campaign-flow-realm".to_owned())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || {
                let path = spec_build_path();
                let source = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
                // The engine strips the `meta` export before it evaluates a
                // flow as a Script. Going through the same function is what
                // keeps this harness reading the code the daemon actually runs.
                let checked = check_script(&source, Some(&path), CheckOptions::default())
                    .expect("spec-build.js must satisfy the flow dialect");
                let mut context = Context::default();
                let args = JsValue::from_json(&args, &mut context).expect("flow args must encode");
                context
                    .register_global_property(js_string!("args"), args, Attribute::READONLY)
                    .expect("args is a fresh global");
                let script = Script::parse(
                    Source::from_bytes(checked.script_source.as_bytes()),
                    None,
                    &mut context,
                )
                .expect("spec-build.js must parse as a Script");
                script
                    .evaluate(&mut context)
                    .expect("evaluating spec-build.js must not throw synchronously");
                ready_tx
                    .send(())
                    .expect("realm creator must still be waiting");

                while let Ok(CampaignFlowRequest::Call {
                    name,
                    arguments,
                    response,
                }) = request_rx.recv()
                {
                    let function = context
                        .global_object()
                        .get(js_string!(name.as_str()), &mut context)
                        .unwrap_or_else(|error| panic!("cannot read global {name}: {error}"))
                        .as_object()
                        .unwrap_or_else(|| panic!("{name} is not a function declaration"))
                        .clone();
                    let arguments = arguments
                        .iter()
                        .map(|value| {
                            JsValue::from_json(value, &mut context).expect("argument encodes")
                        })
                        .collect::<Vec<_>>();
                    let result = function
                        .call(&JsValue::undefined(), &arguments, &mut context)
                        .unwrap_or_else(|error| panic!("calling {name} threw: {error}"));
                    let result = result
                        .to_json(&mut context)
                        .unwrap_or_else(|error| {
                            panic!("{name} returned an undecodable value: {error}")
                        })
                        .unwrap_or(Value::Null);
                    response
                        .send(result)
                        .expect("campaign-flow test must still be waiting");
                }
            })
            .expect("campaign flow test worker must start");
        if ready_rx.recv().is_err() {
            if let Err(panic) = worker.join() {
                std::panic::resume_unwind(panic);
            }
            panic!("campaign flow test worker stopped before realm initialization");
        }
        Self {
            requests: Some(request_tx),
            worker: Some(worker),
        }
    }

    /// Call one top-level helper and decode its result.
    fn call(&mut self, name: &str, arguments: &[Value]) -> Value {
        let (response_tx, response_rx) = mpsc::sync_channel(0);
        self.requests
            .as_ref()
            .expect("campaign flow realm is live")
            .send(CampaignFlowRequest::Call {
                name: name.to_owned(),
                arguments: arguments.to_vec(),
                response: response_tx,
            })
            .expect("campaign flow test worker must be live");
        match response_rx.recv() {
            Ok(value) => value,
            Err(_) => {
                self.requests.take();
                if let Some(worker) = self.worker.take() {
                    if let Err(panic) = worker.join() {
                        std::panic::resume_unwind(panic);
                    }
                }
                panic!("campaign flow test worker stopped before returning {name}");
            }
        }
    }
}

impl Drop for CampaignFlowRealm {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.expect("campaign flow test worker must stop cleanly");
            }
        }
    }
}

fn checkpoint_failure(stage: &str) -> Value {
    json!({
        "task": {"id": "gate", "kind": "checkpoint"},
        "stage": stage,
        "node": {"verdict": "fail"},
    })
}

#[test]
fn local_campaign_dispatch_passes_the_flow_schema_and_binds_the_file_worklist() {
    let args = local_campaign_args("campaign/mecattaf/tally.nix");
    check_campaign_args(&args).unwrap();

    let mut realm = CampaignFlowRealm::new(&args);
    let inputs = realm.call("campaignInputs", &[]);
    assert_eq!(inputs["campaign"], json!("fixture"));
    assert_eq!(inputs["repositoryConfig"]["forge"], json!("local"));
    assert_eq!(inputs["worklist"], json!("specs/ch2.json"));
    assert_eq!(inputs["maxTasks"], json!(1));
    assert_eq!(inputs["maxParallel"], json!(1));

    check_campaign_args(&local_campaign_args("campaign/Acme-Inc/widget_repo.rs")).unwrap();
    check_campaign_args(&local_campaign_args("legacy-runner")).unwrap();
}

#[test]
fn malformed_campaign_namespace_runners_stay_out_of_the_flow_schema() {
    for runner_pool in [
        "campaign/mecattaf",
        "campaign//tally.nix",
        "campaign/./tally.nix",
        "campaign/mecattaf/..",
        "campaign/mecattaf/tally.nix/extra",
        "campaign/mecattaf/tally nix",
    ] {
        let error = check_campaign_args(&local_campaign_args(runner_pool)).unwrap_err();
        assert_eq!(error.code, "args-schema-mismatch");
        assert!(
            error.message.contains("/campaignGraph/manifest/pool"),
            "wrong schema failure for {runner_pool:?}: {error}"
        );
    }
}

/// #337: the deferral arm has to cover the whole deferred lane.
///
/// A checkpoint lane can fail at `prep`, at the checkpoint command itself, and
/// at `checkpoint:record`. The reconciler defers a checkpoint whose verdict
/// unrelated outstanding work can still change, and a deferred lane must spend
/// no budget at any of those three stages: the #308 loop bound terminates by
/// spending the task's retry and steering budget on attempts that mean
/// something, and a deferred pass is not one of them.
#[test]
fn every_stage_of_a_deferred_checkpoint_lane_is_unpriced() {
    let mut realm = CampaignFlowRealm::new(&json!({}));
    let deferred = json!({"deferrals": [{"taskId": "gate", "waitingOn": ["build"]}]});
    for stage in ["prep", "checkpoint", "checkpoint:record"] {
        assert_eq!(
            realm.call(
                "failureClass",
                &[deferred.clone(), checkpoint_failure(stage)]
            ),
            json!("deferred"),
            "a deferred checkpoint lane must not be priced at stage {stage}"
        );
    }
}

/// The same three stages keep their ordinary prices once nothing defers the
/// checkpoint, so the bound still reaches escalation on a checkpoint that
/// genuinely cannot settle. `checkpoint:record` is the stage the #308 base-
/// advanced failure lands on.
#[test]
fn an_undeferred_checkpoint_lane_still_spends_its_budget() {
    let mut realm = CampaignFlowRealm::new(&json!({}));
    let quiet = json!({"deferrals": []});
    assert_eq!(
        realm.call(
            "failureClass",
            &[quiet.clone(), checkpoint_failure("checkpoint")]
        ),
        json!("work")
    );
    for stage in ["prep", "checkpoint:record"] {
        assert_eq!(
            realm.call("failureClass", &[quiet.clone(), checkpoint_failure(stage)]),
            json!("machinery"),
            "stage {stage} is campaign machinery"
        );
    }
    // A deferral naming a different task never reaches this one.
    let elsewhere = json!({"deferrals": [{"taskId": "other", "waitingOn": ["build"]}]});
    assert_eq!(
        realm.call(
            "failureClass",
            &[elsewhere, checkpoint_failure("checkpoint")]
        ),
        json!("work")
    );
}

/// #386: a tree-delta permission breach is priced separately from an
/// ordinary work failure -- it must never be routed through the retry or
/// steering-attempt budget the way a red gate is, because the write already
/// happened and there is nothing to redo.
#[test]
fn a_tree_delta_failure_is_priced_as_a_breach_not_work() {
    let mut realm = CampaignFlowRealm::new(&json!({}));
    let quiet = json!({"deferrals": []});
    let failure = json!({
        "task": {"id": "build", "kind": "implementation"},
        "stage": "treeDelta",
        "node": {"verdict": "fail"},
    });
    assert_eq!(
        realm.call("failureClass", &[quiet, failure]),
        json!("breach")
    );
}

/// #424: the gate refusing to judge a pass is priced as a gate verdict, not as
/// the agent's work being wrong. It gets its own class rather than reusing
/// `breach`, because a receipt saying the task "wrote outside its authorized
/// paths" would be a claim the gate never established -- it could not look.
/// Both classes abort the lane and neither spends a steering attempt.
#[test]
fn a_tree_delta_refusal_is_priced_as_a_gate_verdict_not_as_work_or_a_breach() {
    let mut realm = CampaignFlowRealm::new(&json!({}));
    let quiet = json!({"deferrals": []});
    let failure = json!({
        "task": {"id": "build", "kind": "implementation"},
        "stage": "treeDelta:ungated",
        "node": {"verdict": "fail"},
    });
    assert_eq!(
        realm.call("failureClass", &[quiet.clone(), failure]),
        json!("ungated")
    );
    // The agent stage it stands in for is still `work`, so the two are not
    // being conflated in the other direction either.
    let agent = json!({
        "task": {"id": "build", "kind": "implementation"},
        "stage": "agent",
        "node": {"verdict": "fail"},
    });
    assert_eq!(realm.call("failureClass", &[quiet, agent]), json!("work"));
}

/// An implementation lane is never deferred, whatever the deferral set says.
#[test]
fn an_implementation_lane_is_priced_by_its_stage_alone() {
    let mut realm = CampaignFlowRealm::new(&json!({}));
    let deferred = json!({"deferrals": [{"taskId": "build", "waitingOn": ["other"]}]});
    let failure = json!({
        "task": {"id": "build", "kind": "implementation"},
        "stage": "agent",
        "node": {"verdict": "fail"},
    });
    assert_eq!(
        realm.call("failureClass", &[deferred, failure]),
        json!("work")
    );
}

/// #452: Codex 0.145 could terminate a session immediately after its tool
/// router rejected a destructive command. The router refusal is adapter
/// machinery, not evidence that the requested implementation is wrong, so it
/// must buy the bounded machinery retry before it can consume steering.
#[test]
fn a_codex_tool_router_session_death_is_priced_as_machinery() {
    let mut realm = CampaignFlowRealm::new(&json!({
        "campaign": "fixture",
        "repository": "acme/spec",
        "repositories": {
            "acme/spec": {
                "checkout": "/tmp/fixture",
                "baseBranch": "main",
                "remote": "origin",
                "forge": "local"
            }
        },
        "worklist": "specs/*.json",
        "maxParallel": 1,
        "agent": {
            "adapter": "codex",
            "diagnosisSandboxPolicy": "read-only"
        },
        "gates": []
    }));
    let quiet = json!({"deferrals": []});
    let router_death = json!({
        "task": {"id": "build", "kind": "implementation"},
        "stage": "agent",
        "node": {
            "verdict": "fail",
            "stderrExcerpt": "ERROR codex_core::tools::router: tool call rejected"
        }
    });
    assert_eq!(
        realm.call("failureClass", &[quiet.clone(), router_death]),
        json!("machinery")
    );

    let ordinary_agent_failure = json!({
        "task": {"id": "build", "kind": "implementation"},
        "stage": "agent",
        "node": {"verdict": "fail", "stderrExcerpt": "tests failed"}
    });
    assert_eq!(
        realm.call("failureClass", &[quiet, ordinary_agent_failure]),
        json!("work"),
        "ordinary Codex failures remain task evidence"
    );
}

/// A steward-bound diagnosis node carries no policy vocabulary.
///
/// Witnessed live on eta's first failed-lane diagnosis dispatch: the node was
/// stamped with `sandboxPolicy: "read-only"`, and the steward seam refuses any
/// adapter that declares launch policies at all, so the stamp could never
/// render against a legal steward. The pass died at admission with "invalid
/// adapter narrator: sandboxPolicy value read-only is not authorized by this
/// adapter". The read-only position survives in the node's read-only brief
/// shape; the direct narration subprocess has no jailer to configure.
#[test]
fn a_steward_bound_diagnosis_node_renders_no_policy_vocabulary() {
    let mut realm = CampaignFlowRealm::new(&json!({
        "campaign": "fixture",
        "repository": "acme/spec",
        "repositories": {
            "acme/spec": {
                "checkout": "/tmp/fixture",
                "baseBranch": "main",
                "remote": "origin",
                "forge": "local"
            }
        },
        "worklist": "specs/*.json",
        "maxParallel": 1,
        // Even a campaign that names a diagnosis policy for its worker adapter
        // must not push that name onto the steward.
        "agent": {"adapter": "codex", "diagnosisSandboxPolicy": "read-only"},
        "steward": {
            "adapter": "narrator",
            "argv": ["narrate", "--json"],
            "env": {},
            "finalMessagePattern": "^TALLY_FINAL_MESSAGE=(.*)$",
            "runtimeMaxSec": 120
        },
        "gates": []
    }));
    let bound = realm.call(
        "applyDiagnosisRole",
        &[json!({"id": "diagnose-build", "kind": "diagnosis"})],
    );
    assert_eq!(bound["adapter"], json!("narrator"), "{bound}");
    assert_eq!(bound["priority"], json!("low"), "{bound}");
    assert!(
        bound.get("sandboxPolicy").is_none(),
        "the steward diagnosis node must render no sandbox policy: {bound}"
    );
    assert!(
        bound.get("approvalPolicy").is_none(),
        "the steward diagnosis node must render no approval policy: {bound}"
    );
}

/// Local task-addressed steering composes with campaign-wide steering without
/// leaking between stable task IDs.
#[test]
fn task_steering_composes_the_campaign_and_task_logs() {
    let args = json!({
        "steering": [{"id": 1, "body": "campaign-wide note"}],
        "taskSteering": {
            "alpha": [{"id": 2, "body": "note for alpha"}],
            "beta": [{"id": 3, "body": "note for beta"}],
        },
    });
    let mut realm = CampaignFlowRealm::new(&args);
    let alpha = json!({"id": "alpha", "kind": "implementation"});
    let beta = json!({"id": "beta", "kind": "implementation"});
    assert_eq!(
        realm.call("authorizedComments", &[alpha]),
        json!([{"id": 1, "body": "campaign-wide note"}, {"id": 2, "body": "note for alpha"}])
    );
    assert_eq!(
        realm.call("authorizedComments", &[beta]),
        json!([{"id": 1, "body": "campaign-wide note"}, {"id": 3, "body": "note for beta"}])
    );
    let untargeted = json!({"id": "gamma", "kind": "implementation"});
    assert_eq!(
        realm.call("authorizedComments", &[untargeted]),
        json!([{"id": 1, "body": "campaign-wide note"}])
    );
}
