use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::task::{Poll, Waker};
use std::time::Duration;

use boa_engine::builtins::promise::{OperationType, Promise, PromiseState};
use boa_engine::context::{ContextBuilder, HostHooks};
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::{Attribute, PropertyDescriptor};
use boa_engine::realm::Realm;
use boa_engine::{
    js_string, Context, Finalize, JsData, JsError, JsNativeError, JsObject, JsResult, JsString,
    JsValue, NativeFunction, Script, Source, Trace,
};
use serde_json::{json, Map, Value};

use crate::catalog::sha256;
use crate::dialect::validate_instance;
use crate::error::SupersessionDetails;
use crate::executor::FlowJobExecutor;
use crate::model::{
    flow_canonical_payload_fields, is_nix_store_path, node_spec_fields, sugar_reserved_fields,
    NodeSpecSurface, RunSupersede, SubmissionPlan, NODE_SPEC_INTEGER_FIELDS,
};
use crate::{
    check_script, resolve_members, Admission, Catalog, CheckOptions, Derivation, Disposition,
    FlowClient, FlowError, FlowSubmission, Meta, NodeFailure, NodeResult, NodeSpec, Orchestration,
    RunReport, SelectionProvenance, SelectorOptions, SourceLocation, Verdict, BRIEF_SENTINEL,
    DEFAULT_MAX_NODES, ENGINE_LOOP_LIMIT, ENGINE_MICROTASK_LIMIT, ENGINE_RECURSION_LIMIT,
    ENGINE_WALL_CLOCK_LIMIT, RESULT_PROJECTION_TIMEOUT_CODE, RETRYABLE_PROJECTION_CODE,
};

mod hooks;
mod host;
mod interop;
mod natives;
mod spec;

#[cfg(test)]
mod tests;

use hooks::{CapturedTrace, FlowHooks};
pub(crate) use host::HostShared;
use host::{HostHandle, NodeRevisions};
pub use host::{LifecycleSink, RunOptions, VecLifecycleSink};
pub(crate) use interop::flow_to_js_error;
use interop::{
    apply_captured_trace, call_site, capture_trace, flow_error_value, host, js_error_to_flow,
    value_to_json,
};
use natives::*;
use spec::*;

const BOOTSTRAP: &str = include_str!("bootstrap.js");
const BOOTSTRAP_PATH: &str = "<tally-flow-bootstrap>";
const RUNTIME_ERROR_LOCATION: SourceLocation = SourceLocation::new(1, 1);

/// Validate and execute one flow script against a daemon client.
pub fn run_script(
    source: &str,
    path: Option<&Path>,
    client: Rc<dyn FlowClient>,
    sink: Rc<dyn LifecycleSink>,
    options: RunOptions,
) -> Result<RunReport, FlowError> {
    if options.flow_run_id.trim().is_empty() {
        return Err(FlowError::new(
            "FlowStartupError",
            "flow-run-id-missing",
            "flowRunId must not be empty",
        )
        .at(RUNTIME_ERROR_LOCATION));
    }
    if options.max_nodes == 0 {
        return Err(FlowError::new(
            "FlowStartupError",
            "max-nodes-invalid",
            "--max-nodes must be positive",
        )
        .at(RUNTIME_ERROR_LOCATION));
    }
    if options.catalog.is_some() != options.catalog_hash.is_some() {
        return Err(FlowError::new(
            "FlowCatalogError",
            "catalog-hash-missing",
            "catalog and catalogHash must be supplied together",
        )
        .at(RUNTIME_ERROR_LOCATION));
    }

    let script_hash = sha256(source.as_bytes());
    let args_hash = sha256(&serde_json::to_vec(&options.args).map_err(|error| {
        FlowError::new(
            "FlowStartupError",
            "args-hash-failed",
            format!("cannot hash flow arguments: {error}"),
        )
        .at(RUNTIME_ERROR_LOCATION)
    })?);
    let inspection = futures_lite::future::block_on(client.inspect_run(&options.flow_run_id))
        .map_err(|error| error.into_flow(RUNTIME_ERROR_LOCATION, 0))?;
    // A recorded rollover outranks every hash comparison below. The run was
    // abandoned by an explicit, durable decision, so the honest answer names its
    // successor instead of re-litigating which input moved.
    if let Some(supersede) = inspection.supersede.as_ref() {
        return Err(superseded_error(&options.flow_run_id, supersede));
    }
    let recorded_run = inspection.script_hash.is_some();
    if let Some(recorded_hash) = inspection.script_hash.as_deref() {
        validate_startup_hash(
            "script-changed-mid-run",
            &options.flow_run_id,
            recorded_hash,
            &script_hash,
        )?;
    }
    if let Some(recorded_hash) = inspection.args_hash.as_deref() {
        validate_startup_hash(
            "args-changed-mid-run",
            &options.flow_run_id,
            recorded_hash,
            &args_hash,
        )?;
    }
    if recorded_run && inspection.catalog_hash != options.catalog_hash {
        return Err(changed_catalog_error(
            &options.flow_run_id,
            inspection.catalog_hash.as_deref(),
            options.catalog_hash.as_deref(),
        ));
    }
    let checked = check_script(
        source,
        path,
        CheckOptions {
            args: Some(&options.args),
            catalog: options.catalog.as_ref(),
            catalog_hash: options.catalog_hash.as_deref(),
        },
    )
    .map_err(|error| error.with_ordinal(0))?;

    let effective_max_nodes = checked
        .meta
        .max_nodes
        .map_or(options.max_nodes, |meta| meta.min(options.max_nodes));
    let shared = Rc::new(HostShared {
        client,
        sink,
        meta: checked.meta.clone(),
        flow_run_id: options.flow_run_id,
        script_hash,
        args_hash,
        effective_max_nodes,
        host_call_sites: checked.host_call_sites,
        catalog: options.catalog,
        catalog_hash: options.catalog_hash,
        pool_credentials: options.pool_credentials,
        adapter_skill_revisions: options.adapter_skill_revisions,
        state: RefCell::default(),
    });
    let hooks = Rc::new(FlowHooks::new());
    let executor = Rc::new(FlowJobExecutor::new(
        shared.clone(),
        options.wall_clock_budget,
        options.microtask_budget,
    ));
    let mut context = ContextBuilder::new()
        .host_hooks(hooks.clone())
        .job_executor(executor)
        .build()
        .map_err(|error| {
            FlowError::new(
                "FlowEngineError",
                "engine-initialization",
                format!("cannot initialize Boa: {error}"),
            )
            .at(RUNTIME_ERROR_LOCATION)
        })?;
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(ENGINE_LOOP_LIMIT);
    context
        .runtime_limits_mut()
        .set_recursion_limit(ENGINE_RECURSION_LIMIT);
    context.insert_data(HostHandle {
        shared: shared.clone(),
    });

    harden_engine(&mut context)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    install_host_api(&mut context)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    let args = JsValue::from_json(&options.args, &mut context)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    let meta = JsValue::from_json(&checked.meta_json, &mut context)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    context
        .register_global_property(js_string!("args"), args, Attribute::READONLY)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;
    context
        .register_global_property(js_string!("flowMeta"), meta, Attribute::READONLY)
        .map_err(|error| shared.annotate_frontier(js_error_to_flow(error, &mut context)))?;

    let execution = (|| -> Result<Option<Value>, FlowError> {
        evaluate_script(BOOTSTRAP, Some(Path::new(BOOTSTRAP_PATH)), &mut context)
            .map_err(|error| js_error_to_flow(error, &mut context))?;
        let value = evaluate_script(&checked.script_source, path, &mut context)
            .map_err(|error| js_error_to_flow(error, &mut context))?;
        let root_promise = value
            .as_object()
            .and_then(|object| JsPromise::from_object(object).ok());
        if let Some(promise) = &root_promise {
            hooks.observe_root((**promise).clone());
        }

        if let Err(error) = context.run_jobs() {
            if let Some(fatal) = shared.fatal_error() {
                return Err(fatal);
            }
            return Err(js_error_to_flow(error, &mut context));
        }
        if let Some(fatal) = shared.fatal_error() {
            return Err(fatal);
        }
        let final_js = match root_promise {
            Some(promise) => match promise.state() {
                PromiseState::Fulfilled(value) => value,
                PromiseState::Rejected(reason) => {
                    let mut error = js_error_to_flow(JsError::from_opaque(reason), &mut context);
                    if let Some(trace) = hooks.rejection_trace(&promise) {
                        apply_captured_trace(&mut error, trace);
                    }
                    return Err(error);
                }
                PromiseState::Pending => {
                    return Err(FlowError::new(
                        "FlowPromiseError",
                        "promise-pending",
                        "flow script finished with a promise that can never settle",
                    )
                    .at(RUNTIME_ERROR_LOCATION));
                }
            },
            None => value,
        };
        if let Some(promise) = hooks.unhandled().first() {
            let reason = match JsPromise::from(promise.clone()).state() {
                PromiseState::Rejected(reason) => reason.to_json(&mut context).ok().flatten(),
                PromiseState::Pending | PromiseState::Fulfilled(_) => None,
            };
            let mut error = FlowError::new(
                "FlowUnhandledRejection",
                "unhandled-rejection",
                "flow script left a rejected promise without a handler",
            )
            .at(RUNTIME_ERROR_LOCATION)
            .detail("reason", reason.unwrap_or(Value::Null));
            if let Some(trace) = hooks.rejection_trace(promise) {
                apply_captured_trace(&mut error, trace);
            }
            return Err(error);
        }
        final_js
            .to_json(&mut context)
            .map_err(|error| js_error_to_flow(error, &mut context))
    })()
    .map_err(|error| shared.annotate_frontier(error));

    let flush = shared.flush_final_logs();
    let final_value = match execution {
        Ok(value) => {
            flush?;
            value
        }
        Err(error) => {
            let _ = flush;
            return Err(error);
        }
    };
    let report = shared.report(final_value);
    shared.sink.emit(json!({
        "type": "flow-completed",
        "flowRunId": report.flow_run_id,
        "flowName": report.flow_name,
        "scriptHash": report.script_hash,
        "ordinals": report.ordinal_keys.len(),
    }))?;
    Ok(report)
}

/// Refuse a start whose recorded identity hash and current one disagree.
fn validate_startup_hash(
    code: &str,
    flow_run_id: &str,
    recorded_hash: &str,
    current_hash: &str,
) -> Result<(), FlowError> {
    if recorded_hash == current_hash {
        return Ok(());
    }
    Err(crate::error::supersession_error(
        code,
        format!(
            "flow run {flow_run_id} is pinned to {recorded_hash}, not {current_hash}{}",
            crate::error::identity_refusal_remedy_sentence(code, flow_run_id)
        ),
        &SupersessionDetails {
            flow_run_id,
            recorded_hash: Some(recorded_hash),
            current_hash: Some(current_hash),
            ..SupersessionDetails::default()
        },
    )
    .at(RUNTIME_ERROR_LOCATION))
}

fn changed_catalog_error(
    flow_run_id: &str,
    recorded_hash: Option<&str>,
    current_hash: Option<&str>,
) -> FlowError {
    let rendered_recorded = recorded_hash.unwrap_or("<none>");
    let rendered_current = current_hash.unwrap_or("<none>");
    crate::error::supersession_error(
        "catalog-changed-mid-run",
        format!(
            "flow run {flow_run_id} is pinned to {rendered_recorded}, not {rendered_current}{}",
            crate::error::identity_refusal_remedy_sentence("catalog-changed-mid-run", flow_run_id)
        ),
        &SupersessionDetails {
            flow_run_id,
            recorded_hash,
            current_hash,
            ..SupersessionDetails::default()
        },
    )
    .at(RUNTIME_ERROR_LOCATION)
}

/// Refuse to start a run that a durable rollover already retired.
///
/// This is the one replay refusal that carries its own remedy: the successor is
/// named, so a supervisor switches to it instead of escalating to a human.
fn superseded_error(flow_run_id: &str, supersede: &RunSupersede) -> FlowError {
    crate::error::supersession_error(
        "flow-run-superseded",
        format!(
            "flow run {flow_run_id} was superseded by {} ({}) at {}; run the successor",
            supersede.successor_flow_run_id, supersede.reason, supersede.recorded_at
        ),
        &SupersessionDetails {
            flow_run_id,
            successor_flow_run_id: Some(supersede.successor_flow_run_id.as_str()),
            reason: Some(supersede.reason.as_str()),
            recorded_at: Some(supersede.recorded_at.as_str()),
            ..SupersessionDetails::default()
        },
    )
    .at(RUNTIME_ERROR_LOCATION)
}

fn evaluate_script(source: &str, path: Option<&Path>, context: &mut Context) -> JsResult<JsValue> {
    let script = Script::parse(Source::from_reader(source.as_bytes(), path), None, context)?;
    script.evaluate(context)
}

fn harden_engine(context: &mut Context) -> JsResult<()> {
    let global = context.global_object().clone();
    for name in ["Date", "WeakRef", "FinalizationRegistry", "Iterator"] {
        global.delete_property_or_throw(JsString::from(name), context)?;
    }
    let math = global
        .get(js_string!("Math"), context)?
        .as_object()
        .ok_or_else(|| JsNativeError::typ().with_message("Math global is not an object"))?;
    let random = boa_engine::object::FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_fn_ptr(native_random),
    )
    .name(js_string!("random"))
    .length(0)
    .constructor(false)
    .build();
    math.define_property_or_throw(
        js_string!("random"),
        PropertyDescriptor::builder()
            .value(random)
            .writable(false)
            .enumerable(false)
            .configurable(false),
        context,
    )?;
    Ok(())
}

fn install_host_api(context: &mut Context) -> JsResult<()> {
    for (name, length, function) in [
        ("job", 2, NativeFunction::from_fn_ptr(native_job)),
        ("drv", 2, NativeFunction::from_fn_ptr(native_drv)),
        ("claude", 2, NativeFunction::from_fn_ptr(native_claude)),
        ("codex", 2, NativeFunction::from_fn_ptr(native_codex)),
        ("local", 2, NativeFunction::from_fn_ptr(native_local)),
        ("sh", 2, NativeFunction::from_fn_ptr(native_sh)),
        ("members", 2, NativeFunction::from_fn_ptr(native_members)),
        ("log", 1, NativeFunction::from_fn_ptr(native_log)),
        (
            "__flowError",
            5,
            NativeFunction::from_fn_ptr(native_error_factory),
        ),
        (
            "__flowLocation",
            0,
            NativeFunction::from_fn_ptr(native_location),
        ),
    ] {
        context.register_global_builtin_callable(JsString::from(name), length, function)?;
    }
    Ok(())
}
