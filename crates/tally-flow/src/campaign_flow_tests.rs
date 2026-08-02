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

use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsValue, Script, Source};
use serde_json::{json, Value};

use crate::{check_script, CheckOptions};

fn spec_build_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/flows/spec-build.js")
}

/// One evaluated `spec-build.js` realm bound to a fixed `args`.
struct CampaignFlowRealm {
    context: Context,
}

impl CampaignFlowRealm {
    fn new(args: &Value) -> Self {
        let path = spec_build_path();
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        // The engine strips the `meta` export before it evaluates a flow as a
        // Script. Going through the same function is what keeps this harness
        // reading the code the daemon actually runs.
        let checked = check_script(&source, Some(&path), CheckOptions::default())
            .expect("spec-build.js must satisfy the flow dialect");
        let mut context = Context::default();
        let args = JsValue::from_json(args, &mut context).expect("flow args must encode");
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
        Self { context }
    }

    /// Call one top-level helper and decode its result.
    fn call(&mut self, name: &str, arguments: &[Value]) -> Value {
        let function = self
            .context
            .global_object()
            .get(js_string!(name), &mut self.context)
            .unwrap_or_else(|error| panic!("cannot read global {name}: {error}"))
            .as_object()
            .unwrap_or_else(|| panic!("{name} is not a function declaration"))
            .clone();
        let arguments = arguments
            .iter()
            .map(|value| JsValue::from_json(value, &mut self.context).expect("argument encodes"))
            .collect::<Vec<_>>();
        let result = function
            .call(&JsValue::undefined(), &arguments, &mut self.context)
            .unwrap_or_else(|error| panic!("calling {name} threw: {error}"));
        result
            .to_json(&mut self.context)
            .unwrap_or_else(|error| panic!("{name} returned an undecodable value: {error}"))
            .unwrap_or(Value::Null)
    }
}

fn checkpoint_failure(stage: &str) -> Value {
    json!({
        "task": {"id": "gate", "kind": "checkpoint"},
        "stage": stage,
        "node": {"verdict": "fail"},
    })
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

/// #334 item 5: the per-task steering composition, executed rather than grepped.
///
/// An allowed-actor comment on task A's sub-issue must reach A's brief, a
/// master comment must reach every task, and neither may reach the other task's
/// thread.
#[test]
fn task_steering_composes_the_master_thread_with_the_task_thread() {
    let args = json!({
        "capabilities": {"subIssueWalk": true},
        "steering": [{"id": 1, "body": "campaign-wide note"}],
        "taskSteering": {
            "8": [{"id": 2, "body": "note for alpha"}],
            "9": [{"id": 3, "body": "note for beta"}],
        },
    });
    let mut realm = CampaignFlowRealm::new(&args);
    let alpha = json!({"id": "alpha", "kind": "implementation", "brief": {"issue": {"number": 8}}});
    let beta = json!({"id": "beta", "kind": "implementation", "brief": {"issue": {"number": 9}}});
    assert_eq!(
        realm.call("authorizedComments", &[alpha]),
        json!([{"id": 1, "body": "campaign-wide note"}, {"id": 2, "body": "note for alpha"}])
    );
    assert_eq!(
        realm.call("authorizedComments", &[beta]),
        json!([{"id": 1, "body": "campaign-wide note"}, {"id": 3, "body": "note for beta"}])
    );
    // A task with no sub-issue thread of its own still receives the master.
    let unthreaded = json!({"id": "gamma", "kind": "implementation"});
    assert_eq!(
        realm.call("authorizedComments", &[unthreaded]),
        json!([{"id": 1, "body": "campaign-wide note"}])
    );
}

/// Without the arm-time walk capability there are no task threads at all, so
/// every task sees exactly the master thread even where `taskSteering` was
/// supplied. A degraded campaign must not silently read a native surface.
#[test]
fn a_degraded_campaign_composes_only_the_master_thread() {
    let args = json!({
        "capabilities": {"subIssueWalk": false},
        "steering": [{"id": 1, "body": "campaign-wide note"}],
        "taskSteering": {"8": [{"id": 2, "body": "note for alpha"}]},
    });
    let mut realm = CampaignFlowRealm::new(&args);
    let alpha = json!({"id": "alpha", "kind": "implementation", "brief": {"issue": {"number": 8}}});
    assert_eq!(
        realm.call("authorizedComments", &[alpha]),
        json!([{"id": 1, "body": "campaign-wide note"}])
    );
}
