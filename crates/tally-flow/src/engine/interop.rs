use super::*;

pub(super) fn value_to_json(
    value: &JsValue,
    label: &str,
    context: &mut Context,
) -> JsResult<Value> {
    value.to_json(context)?.ok_or_else(|| {
        JsNativeError::typ()
            .with_message(format!("{label} must be JSON-serializable"))
            .into()
    })
}

pub(super) fn host(context: &Context) -> JsResult<Rc<HostShared>> {
    context
        .get_data::<HostHandle>()
        .map(|handle| handle.shared.clone())
        .ok_or_else(|| {
            JsNativeError::error()
                .with_message("tally-flow host state is unavailable")
                .into()
        })
}

pub(super) fn call_site(context: &Context) -> SourceLocation {
    let mut fallback = None;
    for frame in context.stack_trace() {
        let position = frame.position();
        let Some(position_value) = position.position else {
            continue;
        };
        let location =
            SourceLocation::new(position_value.line_number(), position_value.column_number());
        fallback.get_or_insert(location);
        if !position.path.to_string().contains(BOOTSTRAP_PATH) {
            return context
                .get_data::<HostHandle>()
                .map_or(location, |handle| handle.shared.exact_call_site(location));
        }
    }
    fallback.unwrap_or(RUNTIME_ERROR_LOCATION)
}

pub(super) fn capture_trace(context: &Context) -> Option<CapturedTrace> {
    let mut location = None;
    let mut rendered = Vec::new();
    for frame in context.stack_trace() {
        let position = frame.position();
        let Some(source_position) = position.position else {
            continue;
        };
        let path = position.path.to_string();
        if path.contains(BOOTSTRAP_PATH) {
            continue;
        }
        let frame_location = SourceLocation::new(
            source_position.line_number(),
            source_position.column_number(),
        );
        location.get_or_insert(frame_location);
        let function = position.function_name.to_std_string_escaped();
        let function = if function.is_empty() {
            "<anonymous>".to_owned()
        } else {
            function
        };
        rendered.push(format!(
            "    at {function} ({path}:{}:{})",
            frame_location.line, frame_location.column
        ));
    }
    Some(CapturedTrace {
        location: location?,
        stack: rendered.join("\n"),
    })
}

pub(super) fn apply_captured_trace(error: &mut FlowError, trace: CapturedTrace) {
    if error.location.is_none()
        || (error.location == Some(RUNTIME_ERROR_LOCATION)
            && matches!(
                error.code.as_str(),
                "script-evaluation" | "script-exception" | "unhandled-rejection"
            ))
    {
        error.location = Some(trace.location);
    }
    if error.stack.is_none() && !trace.stack.is_empty() {
        error.stack = Some(trace.stack);
    }
}

fn stack_location(stack: &str) -> Option<SourceLocation> {
    for frame in stack.lines() {
        if !frame.trim_start().starts_with("at ")
            || frame.contains(BOOTSTRAP_PATH)
            || frame.contains("(native at ")
        {
            continue;
        }
        let coordinates = frame.trim().trim_end_matches(')');
        let Some((prefix, column)) = coordinates.rsplit_once(':') else {
            continue;
        };
        let Some((_, line)) = prefix.rsplit_once(':') else {
            continue;
        };
        let (Ok(line), Ok(column)) = (line.parse::<u32>(), column.parse::<u32>()) else {
            continue;
        };
        return Some(SourceLocation::new(line, column));
    }
    None
}

pub(super) fn flow_error_value(error: &FlowError, context: &mut Context) -> JsResult<JsValue> {
    let value = JsError::from_native(JsNativeError::error().with_message(error.message.clone()))
        .into_opaque(context)?;
    let object = value
        .as_object()
        .ok_or_else(|| JsNativeError::error().with_message("cannot construct flow error"))?;
    object.set(
        js_string!("name"),
        JsString::from(error.name.as_str()),
        true,
        context,
    )?;
    object.set(
        js_string!("code"),
        JsString::from(error.code.as_str()),
        true,
        context,
    )?;
    if let Some(location) = error.location {
        object.set(
            js_string!("line"),
            JsValue::from(location.line),
            true,
            context,
        )?;
        object.set(
            js_string!("column"),
            JsValue::from(location.column),
            true,
            context,
        )?;
    }
    if let Some(ordinal) = error.ordinal {
        object.set(
            js_string!("ordinal"),
            JsValue::from(ordinal as f64),
            true,
            context,
        )?;
    }
    let details = JsValue::from_json(&Value::Object(error.details.clone()), context)?;
    object.set(js_string!("details"), details, true, context)?;
    Ok(value)
}

pub(crate) fn flow_to_js_error(error: FlowError, context: &mut Context) -> JsError {
    match flow_error_value(&error, context) {
        Ok(value) => JsError::from_opaque(value),
        Err(_) => JsNativeError::error()
            .with_message(format!(
                "{} [{}]: {}",
                error.name, error.code, error.message
            ))
            .into(),
    }
}

pub(super) fn js_error_to_flow(error: JsError, context: &mut Context) -> FlowError {
    let rendered = error.to_string();
    if matches!(
        error.as_engine(),
        Some(boa_engine::error::EngineError::RuntimeLimit(_))
    ) {
        let location = stack_location(&rendered).unwrap_or(RUNTIME_ERROR_LOCATION);
        return FlowError::new("FlowRuntimeLimitError", "runtime-limit", rendered.clone())
            .at(location)
            .with_stack(rendered);
    }
    let value = match error.into_opaque(context) {
        Ok(value) => value,
        Err(_) => {
            let location = stack_location(&rendered).unwrap_or(RUNTIME_ERROR_LOCATION);
            return FlowError::new("FlowScriptError", "script-exception", rendered.clone())
                .at(location)
                .with_stack(rendered);
        }
    };
    if let Some(object) = value.as_object() {
        fn string_property(object: &JsObject, key: &str, context: &mut Context) -> Option<String> {
            object
                .get(JsString::from(key), context)
                .ok()
                .and_then(|value| value.as_string())
                .map(|value| value.to_std_string_escaped())
        }
        fn number_property(object: &JsObject, key: &str, context: &mut Context) -> Option<f64> {
            object
                .get(JsString::from(key), context)
                .ok()
                .and_then(|value| value.as_number())
        }

        let name = string_property(&object, "name", context)
            .unwrap_or_else(|| "FlowScriptError".to_owned());
        let message =
            string_property(&object, "message", context).unwrap_or_else(|| rendered.clone());
        let code =
            string_property(&object, "code", context).unwrap_or_else(|| match name.as_str() {
                "SyntaxError" => "script-syntax".to_owned(),
                "RangeError" => "runtime-limit".to_owned(),
                "ReferenceError" | "TypeError" | "Error" => "script-evaluation".to_owned(),
                _ => "script-exception".to_owned(),
            });
        let explicit_location = number_property(&object, "line", context)
            .zip(number_property(&object, "column", context))
            .and_then(|(line, column)| {
                Some(SourceLocation::new(
                    u32::try_from(line as u64).ok()?,
                    u32::try_from(column as u64).ok()?,
                ))
            });
        let ordinal = number_property(&object, "ordinal", context).map(|value| value as u64);
        let details = object
            .get(js_string!("details"), context)
            .ok()
            .and_then(|value| value.to_json(context).ok().flatten())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        let stack = string_property(&object, "stack", context)
            .or_else(|| rendered.contains("\n    at ").then(|| rendered.clone()));
        let location = explicit_location
            .or_else(|| stack.as_deref().and_then(stack_location))
            .unwrap_or(RUNTIME_ERROR_LOCATION);
        return FlowError {
            name,
            code,
            message,
            location: Some(location),
            ordinal,
            details,
            stack,
        };
    }
    let location = stack_location(&rendered).unwrap_or(RUNTIME_ERROR_LOCATION);
    FlowError::new("FlowScriptError", "script-exception", rendered.clone())
        .at(location)
        .with_stack(rendered)
}
