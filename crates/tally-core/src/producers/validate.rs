use super::*;

pub fn validate_registry(
    producers: &BTreeMap<String, ProducerConfig>,
    pools: &BTreeSet<String>,
    adapters: &BTreeSet<String>,
    executors: &BTreeSet<String>,
) -> Result<(), ProducerError> {
    for (name, producer) in producers {
        validate_producer_name(name)?;
        validate_credentials(producer.credentials(), &format!("producer {name:?}"))?;
        match producer {
            ProducerConfig::Calendar(config) => {
                if config.on_calendar.trim().is_empty()
                    || config.on_calendar.chars().any(char::is_control)
                {
                    return Err(ProducerError::InvalidConfig(format!(
                        "calendar producer {name:?} requires a non-empty onCalendar"
                    )));
                }
                validate_enqueue(name, "enqueue", &config.enqueue, pools, adapters, executors)?;
            }
            ProducerConfig::EventsDir(config) => {
                if config.poll_interval_sec == 0 {
                    return Err(ProducerError::InvalidConfig(format!(
                        "events-dir producer {name:?} requires positive pollIntervalSec"
                    )));
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_enqueue(
    producer: &str,
    field: &str,
    enqueue: &ProducerEnqueue,
    pools: &BTreeSet<String>,
    adapters: &BTreeSet<String>,
    executors: &BTreeSet<String>,
) -> Result<(), ProducerError> {
    if enqueue.argv.is_empty() {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} argv must not be empty"
        )));
    }
    let mut canonical_pools = enqueue.pools.clone();
    crate::poolset::canonicalize(&mut canonical_pools).map_err(|error| {
        ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} has invalid pool set: {error}"
        ))
    })?;
    for pool in &canonical_pools {
        if !pools.contains(pool) {
            return Err(ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} references unknown pool {pool:?}"
            )));
        }
    }
    if !adapters.contains(&enqueue.adapter) {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} references unknown adapter {:?}",
            enqueue.adapter
        )));
    }
    if let Some(executor) = &enqueue.executor {
        if !executors.contains(executor) {
            return Err(ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} references unknown executor {executor:?}"
            )));
        }
    }
    if enqueue
        .dedup_key
        .as_ref()
        .is_some_and(|key| key.trim().is_empty() || key.chars().any(char::is_control))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} dedupKey must not be empty or contain control characters"
        )));
    }
    if enqueue
        .dedup_key
        .as_deref()
        .is_some_and(|key| StrftimeItems::new(key).any(|item| matches!(item, Item::Error)))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} dedupKey is not a valid strftime template"
        )));
    }
    if enqueue.runtime_max_sec == Some(0) {
        return Err(ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} runtimeMaxSec must be positive"
        )));
    }
    for argument in &enqueue.argv {
        validate_literal_template(argument).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} argv is invalid: {detail}"
            ))
        })?;
    }
    if let Some(brief) = &enqueue.brief {
        validate_literal_value(brief).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} brief is invalid: {detail}"
            ))
        })?;
    }
    if let Some(cwd) = &enqueue.cwd {
        let cwd_text = cwd.to_str().ok_or_else(|| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} cwd must be valid UTF-8"
            ))
        })?;
        validate_literal_template(cwd_text).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} cwd is invalid: {detail}"
            ))
        })?;
        validate_resolved_path(cwd, &format!("producer {producer:?} {field} cwd"))
            .map_err(|error| ProducerError::InvalidConfig(error.to_string()))?;
    }
    if let Some(workspace) = &enqueue.workspace {
        workspace.validate().map_err(|error| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} workspace is invalid: {error}"
            ))
        })?;
    }
    if let Some(gate_manifest) = &enqueue.gate_manifest {
        gate_manifest.validate().map_err(|error| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} gateManifest is invalid: {error}"
            ))
        })?;
    }
    parse_evidence_specs(&enqueue.evidence).map_err(|error| {
        ProducerError::InvalidConfig(format!(
            "producer {producer:?} {field} evidence is invalid: {error}"
        ))
    })?;
    validate_credentials(
        &enqueue.credentials,
        &format!("producer {producer:?} {field}"),
    )?;
    Ok(())
}

pub(super) fn validate_literal_value(value: &Value) -> Result<(), String> {
    match value {
        Value::String(value) => validate_literal_template(value),
        Value::Array(values) => values.iter().try_for_each(validate_literal_value),
        Value::Object(values) => values.iter().try_for_each(|(name, value)| {
            validate_literal_template(name)?;
            validate_literal_value(value)
        }),
        _ => Ok(()),
    }
}

pub(super) fn validate_literal_template(value: &str) -> Result<(), String> {
    if value.contains('\0') {
        return Err("string contains a NUL byte".to_owned());
    }
    if value.contains("${") {
        return Err("placeholders are not supported by this producer kind".to_owned());
    }
    Ok(())
}

pub(super) fn validate_resolved_path(path: &Path, label: &str) -> Result<(), ProducerError> {
    let path = path
        .to_str()
        .ok_or_else(|| ProducerError::InvalidObservation(format!("{label} must be valid UTF-8")))?;
    if !path.starts_with('/')
        || path.contains('%')
        || path.contains('\0')
        || path.chars().any(char::is_control)
    {
        return Err(ProducerError::InvalidObservation(format!(
            "{label}: path must be absolute and contain no control characters or systemd specifiers"
        )));
    }
    Ok(())
}

pub(super) fn validate_producer_name(value: &str) -> Result<(), ProducerError> {
    if value.is_empty()
        || value.len() > MAX_PRODUCER_NAME_BYTES
        || !value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        || matches!(value, "." | "..")
    {
        return Err(ProducerError::InvalidConfig(format!(
            "producer name {value:?} is not a safe file-name component"
        )));
    }
    Ok(())
}

pub(super) fn validate_credentials(
    credentials: &BTreeMap<String, PathBuf>,
    label: &str,
) -> Result<(), ProducerError> {
    for (name, source) in credentials {
        let name_valid = !name.is_empty()
            && name.len() <= 255
            && name != "."
            && name != ".."
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
        if !name_valid {
            return Err(ProducerError::InvalidConfig(format!(
                "{label} has invalid credential name {name:?}"
            )));
        }
        if !source.is_absolute() {
            return Err(ProducerError::InvalidConfig(format!(
                "{label} credential {name:?} source must be absolute"
            )));
        }
        validate_safe_path(source, &format!("{label} credential {name:?}"))?;
    }
    Ok(())
}

pub(super) fn validate_safe_path(path: &Path, label: &str) -> Result<(), ProducerError> {
    let Some(path) = path.to_str() else {
        return Err(ProducerError::InvalidConfig(format!(
            "{label} must be valid UTF-8"
        )));
    };
    if path.is_empty() || path.chars().any(char::is_control) || path.contains('%') {
        return Err(ProducerError::InvalidConfig(format!(
            "{label} must be non-empty and contain neither control characters nor systemd specifiers"
        )));
    }
    Ok(())
}

pub(super) fn expand_dedup_key(
    template: &str,
    now: DateTime<Utc>,
) -> Result<String, ProducerError> {
    if StrftimeItems::new(template).any(|item| matches!(item, Item::Error)) {
        return Err(ProducerError::InvalidObservation(
            "dedupKey is not a valid strftime template".to_owned(),
        ));
    }
    let expanded = now
        .format_with_items(StrftimeItems::new(template))
        .to_string();
    if expanded.trim().is_empty() || expanded.chars().any(char::is_control) {
        return Err(ProducerError::InvalidObservation(
            "strftime-expanded dedupKey is empty or contains control characters".to_owned(),
        ));
    }
    Ok(expanded)
}
