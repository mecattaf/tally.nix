use super::*;

pub(super) fn native_job(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    let raw = value_to_json(
        args.first().unwrap_or(&JsValue::undefined()),
        "job spec",
        context,
    )?;
    let allowed = node_spec_fields(NodeSpecSurface::Job).collect::<Vec<_>>();
    reject_unknown_keys(&raw, &allowed, location)
        .map_err(|error| flow_to_js_error(error, context))?;
    let spec: NodeSpec = serde_json::from_value(raw).map_err(|error| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("job spec has an invalid shape: {error}"),
            )
            .at(location),
            context,
        )
    })?;
    let settle = settle_option(args.get(1), context)?;
    make_job_promise(spec, NodeRevisions::default(), settle, location, context)
}

pub(super) fn native_drv(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    let raw = value_to_json(
        args.first().unwrap_or(&JsValue::undefined()),
        "derivation spec",
        context,
    )?;
    reject_unknown_keys(&raw, &["drvPath", "outputs"], location)
        .map_err(|error| flow_to_js_error(error, context))?;
    let mut drv: Derivation = serde_json::from_value(raw).map_err(|error| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-derivation",
                format!("drv spec has an invalid shape: {error}"),
            )
            .at(location),
            context,
        )
    })?;
    drv.canonicalize().map_err(|error| {
        flow_to_js_error(
            FlowError::new("FlowSpecError", "invalid-derivation", error).at(location),
            context,
        )
    })?;
    let settle = settle_option(args.get(1), context)?;
    let drv_path = drv.drv_path.clone();
    let evidence = drv
        .output_paths()
        .into_iter()
        .map(|path| format!("store:{path}"))
        .collect::<Vec<_>>();
    let spec: NodeSpec = serde_json::from_value(json!({
        "argv": ["nix", "build", "--no-link", format!("{drv_path}^*")],
        "adapter": "shell",
        "pools": ["build"],
        "evidence": evidence,
        "drv": drv,
        "dedupKey": format!("drv:{drv_path}"),
    }))
    .expect("the fixed drv mapping always has a valid NodeSpec shape");
    make_job_promise(spec, NodeRevisions::default(), settle, location, context)
}

pub(super) fn native_claude(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    native_agent_sugar("claude", "claude-code", "claude-window", args, context)
}

pub(super) fn native_codex(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    native_agent_sugar("codex", "codex", "codex-window", args, context)
}

pub(super) fn native_agent_sugar(
    helper: &str,
    adapter: &str,
    pool: &str,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    let prompt = required_string(args.first(), "prompt", location, context)?;
    let (mut options, settle) = sugar_options(args.get(1), location, context)?;
    reject_sugar_conflicts(&options, reserved_fields(helper), location, context)?;
    let revisions = host(context)?.agent_revisions(adapter, &prompt);
    options.insert("adapter".to_owned(), Value::String(adapter.to_owned()));
    options.insert("pools".to_owned(), json!([pool]));
    options.insert("argv".to_owned(), json!([BRIEF_SENTINEL]));
    options.insert("brief".to_owned(), json!({"mission": prompt}));
    let spec = decode_sugar_spec(options, location, context)?;
    make_job_promise(spec, revisions, settle, location, context)
}

pub(super) fn native_sh(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    let argv = value_to_json(
        args.first().unwrap_or(&JsValue::undefined()),
        "shell argv",
        context,
    )?;
    let (mut options, settle) = sugar_options(args.get(1), location, context)?;
    reject_sugar_conflicts(&options, reserved_fields("sh"), location, context)?;
    options.insert("argv".to_owned(), argv);
    options.insert("adapter".to_owned(), Value::String("shell".to_owned()));
    let spec = decode_sugar_spec(options, location, context)?;
    make_job_promise(spec, NodeRevisions::default(), settle, location, context)
}

pub(super) fn native_local(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    let prompt = required_string(args.first(), "prompt", location, context)?;
    let (mut options, settle) = sugar_options(args.get(1), location, context)?;
    let mut allowed = node_spec_fields(NodeSpecSurface::Sugar).collect::<Vec<_>>();
    allowed.push("member");
    reject_unknown_keys(&Value::Object(options.clone()), &allowed, location)
        .map_err(|error| flow_to_js_error(error, context))?;
    reject_sugar_conflicts(&options, reserved_fields("local"), location, context)?;
    let member_value = options.remove("member").ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowSelectorError",
                "member-required",
                "local(prompt, opts) requires opts.member from members()",
            )
            .at(location),
            context,
        )
    })?;
    let member_id = match &member_value {
        Value::String(id) => id.clone(),
        Value::Object(object) => object
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                flow_to_js_error(
                    FlowError::new(
                        "FlowSelectorError",
                        "member-invalid",
                        "opts.member must be a catalog member object or member id",
                    )
                    .at(location),
                    context,
                )
            })?,
        _ => {
            return Err(flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "member-invalid",
                    "opts.member must be a catalog member object or member id",
                )
                .at(location),
                context,
            ));
        }
    };
    let shared = host(context)?;
    let catalog = shared.catalog.as_ref().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowCatalogError",
                "catalog-required",
                "local() requires a selector catalog",
            )
            .at(location),
            context,
        )
    })?;
    let member = catalog
        .members
        .iter()
        .find(|candidate| candidate.id == member_id)
        .ok_or_else(|| {
            flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "member-unknown",
                    format!("catalog has no member {member_id:?}"),
                )
                .at(location),
                context,
            )
        })?
        .clone();
    let selection_value = member_value
        .as_object()
        .and_then(|object| object.get("selection"))
        .cloned()
        .ok_or_else(|| {
            flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "selection-provenance-missing",
                    "local() member did not come from this run's members() result",
                )
                .at(location),
                context,
            )
        })?;
    let selection: SelectionProvenance =
        serde_json::from_value(selection_value).map_err(|error| {
            flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "selection-provenance-invalid",
                    format!("member selection provenance is invalid: {error}"),
                )
                .at(location),
                context,
            )
        })?;
    if selection.member_id != member.id
        || shared.catalog_hash.as_deref() != Some(selection.catalog_hash.as_str())
        || !shared.selection_was_resolved(&selection)
    {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSelectorError",
                "selection-provenance-invalid",
                "member selection provenance does not match the active catalog",
            )
            .at(location),
            context,
        ));
    }

    let revisions = shared.agent_revisions(&member.adapter, &prompt);
    options.insert("adapter".to_owned(), Value::String(member.adapter));
    options.insert(
        "pools".to_owned(),
        serde_json::to_value(member.pools).expect("a string vector always serializes"),
    );
    options.insert("argv".to_owned(), json!([BRIEF_SENTINEL]));
    options.insert("brief".to_owned(), json!({"mission": prompt}));
    let launch = member.launch.as_object().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowCatalogError",
                "catalog-launch-invalid",
                "catalog member launch must be an object",
            )
            .at(location),
            context,
        )
    })?;
    options.insert("adapterOptions".to_owned(), Value::Object(launch.clone()));
    options.insert(
        "selection".to_owned(),
        serde_json::to_value(selection).expect("selection provenance always serializes"),
    );
    let spec: NodeSpec = serde_json::from_value(Value::Object(options)).map_err(|error| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("local() options have an invalid shape: {error}"),
            )
            .at(location),
            context,
        )
    })?;
    make_job_promise(spec, revisions, settle, location, context)
}

pub(super) fn native_members(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    let selector = required_string(args.first(), "selector", location, context)?;
    let opts_value = args.get(1).cloned().unwrap_or_else(JsValue::undefined);
    let options = if opts_value.is_undefined() {
        SelectorOptions::default()
    } else {
        let value = value_to_json(&opts_value, "selector options", context)?;
        serde_json::from_value(value).map_err(|error| {
            flow_to_js_error(
                FlowError::new(
                    "FlowSelectorError",
                    "selector-invalid-options",
                    format!("members() options are invalid: {error}"),
                )
                .at(location),
                context,
            )
        })?
    };
    let shared = host(context)?;
    if !shared.meta.selectors.iter().any(|item| item == &selector) {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSelectorError",
                "selector-undeclared",
                format!("selector {selector:?} is absent from meta.selectors"),
            )
            .at(location)
            .detail("selector", selector),
            context,
        ));
    }
    let catalog = shared.catalog.as_ref().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowCatalogError",
                "catalog-required",
                "members() requires --catalog",
            )
            .at(location),
            context,
        )
    })?;
    let catalog_hash = shared.catalog_hash.as_deref().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowCatalogError",
                "catalog-hash-missing",
                "members() requires the content hash of its catalog",
            )
            .at(location),
            context,
        )
    })?;
    let selection = resolve_members(catalog, catalog_hash, &selector, &options)
        .map_err(|error| flow_to_js_error(error.at(location), context))?;
    let member_ids = selection
        .members
        .iter()
        .map(|member| member.id.clone())
        .collect::<Vec<_>>();
    shared
        .sink
        .emit(json!({
            "type": "selector-resolved",
            "flowRunId": shared.flow_run_id,
            "selector": selector,
            "opts": options,
            "catalogHash": catalog_hash,
            "members": member_ids,
        }))
        .map_err(|error| flow_to_js_error(error.at(location), context))?;
    shared.record_selection(&selector, catalog_hash, &member_ids);
    let rows = selection
        .members
        .into_iter()
        .map(|member| {
            let provenance = SelectionProvenance {
                selector: selection.selector.clone(),
                catalog_hash: selection.catalog_hash.clone(),
                member_id: member.id.clone(),
                members: member_ids.clone(),
            };
            let mut value =
                serde_json::to_value(member).expect("a catalog member always serializes");
            value
                .as_object_mut()
                .expect("a catalog member serializes to an object")
                .insert(
                    "selection".to_owned(),
                    serde_json::to_value(provenance)
                        .expect("selection provenance always serializes"),
                );
            value
        })
        .collect::<Vec<_>>();
    JsValue::from_json(&Value::Array(rows), context)
}

pub(super) fn native_log(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    let message = args
        .first()
        .unwrap_or(&JsValue::undefined())
        .to_json(context)?
        .unwrap_or(Value::Null);
    host(context)?.queue_log(message, location);
    Ok(JsValue::undefined())
}

pub(super) fn native_random(
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    Err(flow_to_js_error(
        FlowError::determinism(
            "Math.random",
            "Math.random is forbidden because it would break replay",
            call_site(context),
        ),
        context,
    ))
}

pub(super) fn native_error_factory(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let mut location = call_site(context);
    let name = required_string(args.first(), "error name", location, context)?;
    let code = required_string(args.get(1), "error code", location, context)?;
    let message = required_string(args.get(2), "error message", location, context)?;
    let mut error = FlowError::new(name, code, message).at(location);
    if let Some(details) = args.get(3) {
        if let Some(Value::Object(map)) = details.to_json(context)? {
            error.details = map;
        }
    }
    if let Some(position) = args
        .get(4)
        .and_then(|value| value.to_json(context).ok().flatten())
    {
        if let (Some(line), Some(column)) = (
            position.get("line").and_then(Value::as_u64),
            position.get("column").and_then(Value::as_u64),
        ) {
            location = SourceLocation::new(
                u32::try_from(line).unwrap_or(u32::MAX),
                u32::try_from(column).unwrap_or(u32::MAX),
            );
            error.location = Some(location);
        }
    }
    flow_error_value(&error, context)
}

pub(super) fn native_location(
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let location = call_site(context);
    JsValue::from_json(
        &json!({"line": location.line, "column": location.column}),
        context,
    )
}

pub(super) fn make_job_promise(
    spec: NodeSpec,
    revisions: NodeRevisions,
    settle: bool,
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<JsValue> {
    let shared = host(context)?;
    let plan = shared
        .prepare_submission(spec, revisions, settle, location)
        .map_err(|error| flow_to_js_error(error, context))?;
    let promise = JsPromise::from_async_fn(
        async move |context| match shared.execute_submission(plan).await {
            Ok(result) => {
                let value = serde_json::to_value(result).map_err(|error| {
                    JsNativeError::error()
                        .with_message(format!("cannot serialize NodeResult: {error}"))
                })?;
                JsValue::from_json(&value, &mut context.borrow_mut())
            }
            Err(error) => Err(flow_to_js_error(error, &mut context.borrow_mut())),
        },
        context,
    );
    Ok(promise.into())
}

pub(super) fn sugar_options(
    value: Option<&JsValue>,
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<(Map<String, Value>, bool)> {
    let Some(value) = value else {
        return Ok((Map::new(), false));
    };
    if value.is_undefined() {
        return Ok((Map::new(), false));
    }
    let value = value_to_json(value, "sugar options", context)?;
    let mut options = value.as_object().cloned().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-options",
                "sugar options must be an object",
            )
            .at(location),
            context,
        )
    })?;
    let settle = options
        .remove("settle")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                flow_to_js_error(
                    FlowError::new(
                        "FlowSpecError",
                        "invalid-options",
                        "opts.settle must be boolean",
                    )
                    .at(location),
                    context,
                )
            })
        })
        .transpose()?
        .unwrap_or(false);
    Ok((options, settle))
}

pub(super) fn decode_sugar_spec(
    options: Map<String, Value>,
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<NodeSpec> {
    let allowed = node_spec_fields(NodeSpecSurface::Sugar).collect::<Vec<_>>();
    reject_unknown_keys(&Value::Object(options.clone()), &allowed, location)
        .map_err(|error| flow_to_js_error(error, context))?;
    serde_json::from_value(Value::Object(options)).map_err(|error| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("sugar options have an invalid shape: {error}"),
            )
            .at(location),
            context,
        )
    })
}

fn reserved_fields(helper: &str) -> &'static [&'static str] {
    sugar_reserved_fields(helper).expect("every sugar native names a known helper")
}

pub(super) fn reject_sugar_conflicts(
    options: &Map<String, Value>,
    reserved: &[&str],
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<()> {
    if let Some(field) = options
        .keys()
        .find(|field| reserved.contains(&field.as_str()))
    {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "sugar-option-conflict",
                format!("sugar option {field:?} is fixed by its adapter preset"),
            )
            .at(location)
            .detail("field", field.clone()),
            context,
        ));
    }
    Ok(())
}

pub(super) fn settle_option(value: Option<&JsValue>, context: &mut Context) -> JsResult<bool> {
    let Some(value) = value else {
        return Ok(false);
    };
    if value.is_undefined() {
        return Ok(false);
    }
    let location = call_site(context);
    let value = value_to_json(value, "job options", context)?;
    let object = value.as_object().ok_or_else(|| {
        flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-options",
                "job options must be an object",
            )
            .at(location),
            context,
        )
    })?;
    for key in object.keys() {
        if key != "settle" {
            return Err(flow_to_js_error(
                FlowError::new(
                    "FlowSpecError",
                    "invalid-options",
                    format!("unknown job option {key:?}"),
                )
                .at(location),
                context,
            ));
        }
    }
    object
        .get("settle")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                flow_to_js_error(
                    FlowError::new(
                        "FlowSpecError",
                        "invalid-options",
                        "job option settle must be boolean",
                    )
                    .at(location),
                    context,
                )
            })
        })
        .transpose()
        .map(Option::unwrap_or_default)
}
