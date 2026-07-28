use super::*;

pub(super) fn validate_node_spec_shape(
    spec: &NodeSpec,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let has_argv = spec.argv.is_some();
    let has_prompt = spec.prompt.is_some();
    if has_argv == has_prompt {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-spec",
            "job spec requires exactly one of argv or adapter+prompt",
        )
        .at(location));
    }
    if has_prompt && spec.adapter.as_deref().is_none_or(str::is_empty) {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-spec",
            "job spec with prompt requires a non-empty adapter",
        )
        .at(location));
    }
    if let Some(argv) = &spec.argv {
        if argv.is_empty()
            || argv.first().is_some_and(String::is_empty)
            || argv.iter().any(|argument| argument.contains('\0'))
        {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-argv",
                "argv requires a non-empty executable and may not contain NUL bytes",
            )
            .at(location));
        }
    }
    for (name, value) in [
        ("adapter", spec.adapter.as_deref()),
        ("executor", spec.executor.as_deref()),
    ] {
        if value.is_some_and(|value| value.is_empty() || value.chars().any(char::is_control)) {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("{name} must be non-empty and contain no control characters"),
            )
            .at(location));
        }
    }
    if spec.runtime_max_sec == Some(0) {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-runtime",
            "runtimeMaxSec must be positive",
        )
        .at(location));
    }
    if spec.brief.as_ref().is_some_and(|brief| !brief.is_object()) {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-brief",
            "brief must be a structured JSON object",
        )
        .at(location));
    }
    if let Some(priority) = &spec.priority {
        if !matches!(priority.as_str(), "interrupt" | "high" | "medium" | "low") {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-priority",
                format!("priority {priority:?} is not a tally priority"),
            )
            .at(location));
        }
    }
    for (name, value) in [
        ("key", spec.key.as_deref()),
        ("dedupKey", spec.dedup_key.as_deref()),
        ("label", spec.label.as_deref()),
    ] {
        if value.is_some_and(|value| value.is_empty() || value.chars().any(char::is_control)) {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-spec",
                format!("{name} must be non-empty and contain no control characters"),
            )
            .at(location));
        }
    }
    for (name, value) in &spec.env {
        validate_environment_entry(name, value, location)?;
    }
    if let Some(drv) = &spec.drv {
        drv.validate().map_err(|error| {
            FlowError::new("FlowSpecError", "invalid-derivation", error).at(location)
        })?;
        let expected_argv = vec![
            "nix".to_owned(),
            "build".to_owned(),
            "--no-link".to_owned(),
            format!("{}^*", drv.drv_path),
        ];
        let expected_evidence = drv
            .output_paths()
            .into_iter()
            .map(|path| format!("store:{path}"))
            .collect::<Vec<_>>();
        let expected_key = format!("drv:{}", drv.drv_path);
        if spec.argv.as_ref() != Some(&expected_argv)
            || spec.adapter.as_deref() != Some("shell")
            || spec.pools != ["build"]
            || spec.evidence != expected_evidence
            || spec.dedup_key.as_deref() != Some(expected_key.as_str())
        {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-derivation",
                "drv nodes require the fixed nix build argv, shell adapter, build pool, drv key, and output store evidence",
            )
            .at(location));
        }
    }
    Ok(())
}

pub(super) fn normalize_workspace(
    spec: &mut NodeSpec,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let Some(workspace) = spec.workspace.take() else {
        return Ok(());
    };
    let object = workspace.as_object().ok_or_else(|| {
        FlowError::new(
            "FlowSpecError",
            "invalid-workspace",
            "workspace must be an object",
        )
        .at(location)
    })?;
    const FIELDS: [&str; 4] = ["repo", "baseRev", "branch", "worktreePath"];
    if let Some(field) = object
        .keys()
        .find(|field| !FIELDS.contains(&field.as_str()))
    {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-workspace",
            format!("unknown workspace field {field:?}"),
        )
        .at(location)
        .detail("field", field.clone()));
    }
    let mut normalized = Map::new();
    for field in FIELDS {
        let value = object.get(field).and_then(Value::as_str).ok_or_else(|| {
            FlowError::new(
                "FlowSpecError",
                "invalid-workspace",
                format!("workspace.{field} must be a string"),
            )
            .at(location)
        })?;
        if value.trim().is_empty() || value.contains('\0') || value.chars().any(char::is_control) {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-workspace",
                format!("workspace.{field} must be non-empty and contain no control characters"),
            )
            .at(location));
        }
        if field == "worktreePath" && !value.starts_with('/') {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-workspace",
                "workspace.worktreePath must be absolute",
            )
            .at(location));
        }
        normalized.insert(field.to_owned(), Value::String(value.to_owned()));
    }
    spec.workspace = Some(Value::Object(normalized));
    Ok(())
}

pub(super) fn validate_environment_entry(
    name: &str,
    value: &str,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let mut bytes = name.bytes();
    let valid_name = bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    // Invalid and reserved are different author mistakes: one is a typo in the
    // name, the other is a name the host owns. The code stays `reserved-env`.
    if !valid_name {
        return Err(FlowError::new(
            "FlowEnvironmentError",
            "reserved-env",
            format!(
                "environment name {name:?} is not a valid name: it must start with a letter or \
                 underscore and contain only letters, digits, and underscores"
            ),
        )
        .at(location)
        .detail("name", name.to_owned())
        .detail("reason", "invalid"));
    }
    if name.starts_with("TALLY_") || name == "CREDENTIALS_DIRECTORY" {
        return Err(FlowError::new(
            "FlowEnvironmentError",
            "reserved-env",
            format!(
                "environment name {name:?} is reserved by the host: names beginning TALLY_ and \
                 CREDENTIALS_DIRECTORY are set for the job, not by it"
            ),
        )
        .at(location)
        .detail("name", name.to_owned())
        .detail("reason", "reserved"));
    }
    if value.contains('\0') {
        return Err(FlowError::new(
            "FlowEnvironmentError",
            "invalid-env-value",
            format!("environment value for {name:?} contains a NUL byte"),
        )
        .at(location)
        .detail("name", name.to_owned()));
    }
    Ok(())
}

pub(super) fn normalize_prompt(
    spec: &mut NodeSpec,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let Some(prompt) = spec.prompt.take() else {
        return Ok(());
    };
    if prompt.is_empty() {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-prompt",
            "prompt must not be empty",
        )
        .at(location));
    }
    if spec.brief.is_some() {
        return Err(FlowError::new(
            "FlowSpecError",
            "brief-conflict",
            "adapter+prompt cannot also declare brief",
        )
        .at(location));
    }
    spec.argv = Some(vec![BRIEF_SENTINEL.to_owned()]);
    spec.brief = Some(json!({"mission": prompt}));
    Ok(())
}

pub(super) fn normalize_pools(
    spec: &mut NodeSpec,
    meta: &Meta,
    location: SourceLocation,
) -> Result<(), FlowError> {
    if spec.pools.is_empty() {
        return Err(FlowError::new(
            "FlowPoolError",
            "empty-pools",
            "every flow node must request at least one pool",
        )
        .at(location));
    }
    let mut seen = HashSet::new();
    for pool in &spec.pools {
        if pool.trim().is_empty() || pool.chars().any(char::is_control) {
            return Err(FlowError::new(
                "FlowPoolError",
                "invalid-pool",
                format!("invalid pool name {pool:?}"),
            )
            .at(location));
        }
        if !seen.insert(pool.clone()) {
            return Err(FlowError::new(
                "FlowPoolError",
                "duplicate-pool",
                format!("pool {pool:?} appears more than once"),
            )
            .at(location));
        }
        if spec.drv.is_none() && !meta.pools.iter().any(|declared| declared == pool) {
            return Err(FlowError::new(
                "FlowPoolError",
                "undeclared-pool",
                format!("pool {pool:?} is absent from meta.pools"),
            )
            .at(location)
            .detail("pool", pool.clone()));
        }
    }
    spec.pools.sort();
    spec.evidence = canonicalize_evidence(&spec.evidence, location)?;
    Ok(())
}

pub(super) fn canonicalize_evidence(
    evidence: &[String],
    location: SourceLocation,
) -> Result<Vec<String>, FlowError> {
    let invalid = |message: String| {
        FlowError::new("FlowEvidenceError", "invalid-evidence", message).at(location)
    };
    let mut hash_seen = false;
    let mut exit_seen = false;
    let mut store_paths = BTreeSet::new();
    let mut canonical = Vec::with_capacity(evidence.len());
    for spec in evidence {
        let (kind, value) = spec
            .split_once(':')
            .ok_or_else(|| invalid(format!("evidence spec must use <kind>:<value>: {spec:?}")))?;
        match kind {
            "artifact" if !value.is_empty() => canonical.push(spec.clone()),
            "artifact" => {
                return Err(invalid("artifact evidence requires a path".to_owned()));
            }
            "store" if !is_nix_store_path(value) => {
                return Err(invalid(
                    "store evidence requires an absolute canonical Nix store path".to_owned(),
                ));
            }
            "store" if !store_paths.insert(value.to_owned()) => {
                return Err(invalid(format!(
                    "store evidence contains duplicate path {value}"
                )));
            }
            "store" => canonical.push(spec.clone()),
            "hash" => {
                if hash_seen {
                    return Err(invalid("hash evidence appears more than once".to_owned()));
                }
                let (algorithm, expected) = value
                    .split_once(':')
                    .map_or((value, None), |(algorithm, expected)| {
                        (algorithm, Some(expected))
                    });
                if algorithm != "sha256" {
                    return Err(invalid(format!(
                        "unsupported evidence hash algorithm {algorithm:?}"
                    )));
                }
                let rendered = if let Some(expected) = expected {
                    let hex = expected.strip_prefix("sha256:").unwrap_or(expected);
                    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        return Err(invalid(
                            "a fixed sha256 value must contain exactly 64 hexadecimal digits"
                                .to_owned(),
                        ));
                    }
                    format!("hash:sha256:{}", hex.to_ascii_lowercase())
                } else {
                    "hash:sha256".to_owned()
                };
                canonical.push(rendered);
                hash_seen = true;
            }
            "exit" => {
                if exit_seen {
                    return Err(invalid("exit evidence appears more than once".to_owned()));
                }
                let code = value.parse::<i32>().map_err(|_| {
                    invalid(format!("exit evidence requires an integer code: {spec:?}"))
                })?;
                if !(0..=255).contains(&code) {
                    return Err(invalid(format!(
                        "exit evidence code must be in 0..=255: {code}"
                    )));
                }
                canonical.push(format!("exit:{code}"));
                exit_seen = true;
            }
            _ => {
                return Err(invalid(format!(
                    "unknown evidence kind {kind:?}; expected artifact, store, hash, or exit"
                )));
            }
        }
    }
    Ok(canonical)
}

pub(super) fn normalize_adapter_options(
    spec: &mut NodeSpec,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let mut options = match spec.adapter_options.take() {
        Some(Value::Object(options)) => options,
        Some(_) => {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-adapter-options",
                "adapter options must be an object",
            )
            .at(location));
        }
        None => Map::new(),
    };
    let allowed = [
        "prePromptArgv",
        "environment",
        "approvalPolicy",
        "sandboxPolicy",
        "model",
        "effort",
    ];
    if let Some(field) = options
        .keys()
        .find(|field| !allowed.contains(&field.as_str()))
    {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            format!("unknown adapter option {field:?}"),
        )
        .at(location)
        .detail("field", field.clone()));
    }

    let pre_prompt_argv = options
        .remove("prePromptArgv")
        .unwrap_or_else(|| Value::Array(Vec::new()));
    if pre_prompt_argv
        .as_array()
        .is_none_or(|items| items.iter().any(|item| !item.is_string()))
    {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            "adapterOptions.prePromptArgv must be an array of strings",
        )
        .at(location));
    }
    let supplied_environment = options
        .remove("environment")
        .unwrap_or_else(|| Value::Object(Map::new()));
    let mut environment = supplied_environment.as_object().cloned().ok_or_else(|| {
        FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            "adapterOptions.environment must be an object",
        )
        .at(location)
    })?;
    if environment.values().any(|value| !value.is_string()) {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            "adapterOptions.environment values must be strings",
        )
        .at(location));
    }
    for (name, value) in &environment {
        validate_environment_entry(
            name,
            value
                .as_str()
                .expect("adapter environment values were checked as strings"),
            location,
        )?;
    }
    if pre_prompt_argv
        .as_array()
        .expect("pre-prompt argv was checked as an array")
        .iter()
        .any(|value| {
            value
                .as_str()
                .expect("pre-prompt argv items were checked as strings")
                .contains('\0')
        })
    {
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-adapter-options",
            "adapterOptions.prePromptArgv may not contain NUL bytes",
        )
        .at(location));
    }
    for field in ["approvalPolicy", "sandboxPolicy", "model", "effort"] {
        if options.get(field).is_some_and(|value| !value.is_string()) {
            return Err(FlowError::new(
                "FlowSpecError",
                "invalid-adapter-options",
                format!("adapterOptions.{field} must be a string"),
            )
            .at(location));
        }
    }
    for (name, value) in std::mem::take(&mut spec.env) {
        if environment
            .insert(name.clone(), Value::String(value))
            .is_some()
        {
            return Err(FlowError::new(
                "FlowSpecError",
                "duplicate-environment",
                format!("environment variable {name:?} is set twice"),
            )
            .at(location));
        }
    }
    let mut environment_entries = environment.into_iter().collect::<Vec<_>>();
    environment_entries.sort_by(|left, right| left.0.cmp(&right.0));
    let environment = environment_entries.into_iter().collect::<Map<_, _>>();
    let mut normalized = Map::new();
    normalized.insert("prePromptArgv".to_owned(), pre_prompt_argv);
    normalized.insert("environment".to_owned(), Value::Object(environment));
    for field in ["approvalPolicy", "sandboxPolicy", "model", "effort"] {
        if let Some(value) = options.remove(field) {
            normalized.insert(field.to_owned(), value);
        }
    }
    spec.adapter_options = Some(Value::Object(normalized));
    Ok(())
}

pub(super) fn resolve_pool_credentials(
    pools: &[String],
    configured: &BTreeMap<String, BTreeMap<String, PathBuf>>,
) -> BTreeMap<String, PathBuf> {
    let mut credentials = BTreeMap::new();
    for pool in pools {
        if let Some(pool_credentials) = configured.get(pool) {
            for (name, path) in pool_credentials {
                credentials
                    .entry(name.clone())
                    .or_insert_with(|| path.clone());
            }
        }
    }
    credentials
}

pub(super) fn canonical_payload_hash(
    spec: &NodeSpec,
    credentials: &BTreeMap<String, PathBuf>,
) -> Result<String, FlowError> {
    // Flow-side counterpart to tally_core::wire::CanonicalPayload. The NodeSpec
    // contract owns the field names and ordering; the cross-crate structural
    // guard fails if the kernel hashes a field this builder does not classify.
    let mut payload = Map::new();
    if let Some(argv) = &spec.argv {
        payload.insert("argv".to_owned(), json!(argv));
    }
    payload.insert(
        "pool".to_owned(),
        match spec.pools.as_slice() {
            [pool] => Value::String(pool.clone()),
            pools => json!(pools),
        },
    );
    if let Some(executor) = &spec.executor {
        payload.insert("executor".to_owned(), json!(executor));
    }
    if let Some(adapter) = &spec.adapter {
        payload.insert("adapter".to_owned(), json!(adapter));
    }
    if let Some(workspace) = &spec.workspace {
        payload.insert("workspace".to_owned(), workspace.clone());
    }
    payload.insert(
        "adapterOptions".to_owned(),
        spec.adapter_options
            .clone()
            .unwrap_or_else(|| json!({"prePromptArgv": [], "environment": {}})),
    );
    payload.insert("evidence".to_owned(), json!(spec.evidence));
    if let Some(drv) = &spec.drv {
        payload.insert("drv".to_owned(), json!(drv));
    }
    if let Some(class) = &spec.evidence_class {
        payload.insert("evidenceClass".to_owned(), class.clone());
    }
    if let Some(hash) = &spec.manifest_hash {
        payload.insert("manifestHash".to_owned(), json!(hash));
    }
    if let Some(runtime) = spec.runtime_max_sec {
        payload.insert("runtimeMaxSec".to_owned(), json!(runtime));
    }
    payload.insert("noEnqueue".to_owned(), Value::Bool(true));
    payload.insert(
        "credentials".to_owned(),
        serde_json::to_value(credentials).map_err(|error| {
            FlowError::new(
                "FlowSpecError",
                "payload-serialization",
                format!("cannot serialize resolved pool credentials: {error}"),
            )
        })?,
    );
    if let Some(brief) = &spec.brief {
        let bytes = serde_json::to_vec(brief).map_err(|error| {
            FlowError::new(
                "FlowSpecError",
                "brief-serialization",
                format!("cannot serialize brief: {error}"),
            )
        })?;
        payload.insert("briefHash".to_owned(), Value::String(sha256(&bytes)));
    }
    let mut ordered = Map::new();
    for field in flow_canonical_payload_fields() {
        if let Some(value) = payload.remove(field) {
            ordered.insert(field.to_owned(), value);
        }
    }
    if let Some(field) = payload.keys().next() {
        return Err(FlowError::new(
            "FlowSpecError",
            "canonical-field-unclassified",
            format!("canonical payload field {field:?} is absent from the NodeSpec contract"),
        ));
    }
    let bytes = serde_json::to_vec(&Value::Object(ordered)).map_err(|error| {
        FlowError::new(
            "FlowSpecError",
            "payload-serialization",
            format!("cannot serialize canonical payload: {error}"),
        )
    })?;
    Ok(sha256(&bytes))
}

pub(super) fn stable_drv_task_uuid(flow_run_id: &str, ordinal: u64) -> String {
    let digest = sha256(format!("flow:{flow_run_id}:{ordinal}").as_bytes());
    let hex = digest
        .strip_prefix("sha256:")
        .expect("sha256 helper always returns a tagged digest");
    let mut bytes = [0_u8; 16];
    for (index, pair) in hex.as_bytes().chunks_exact(2).take(16).enumerate() {
        let pair = std::str::from_utf8(pair).expect("sha256 output is ASCII");
        bytes[index] =
            u8::from_str_radix(pair, 16).expect("sha256 output contains hexadecimal digits");
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

pub(super) fn validate_terminal_result(
    result: &NodeResult,
    admission: &crate::Admission,
    location: SourceLocation,
    ordinal: u64,
) -> Result<(), FlowError> {
    if result.task_uuid != admission.task_uuid {
        return Err(FlowError::new(
            "FlowProtocolError",
            "terminal-task-mismatch",
            format!(
                "terminal result names task {} but admission named {}",
                result.task_uuid, admission.task_uuid
            ),
        )
        .at(location)
        .with_ordinal(ordinal));
    }
    if result.witness_seq == 0 {
        return Err(FlowError::new(
            "FlowProtocolError",
            "terminal-witness-invalid",
            "terminal result witnessSeq must be positive",
        )
        .at(location)
        .with_ordinal(ordinal));
    }
    Ok(())
}

/// Which object a rejected field came from.
///
/// The call sites take different field sets, and an author calls a sugar's second
/// argument options rather than a spec, so `unknown-spec-field` names the surface
/// it means instead of calling every one of them a job spec.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SpecSurface<'a> {
    JobSpec,
    DerivationSpec,
    SugarOptions(&'a str),
}

impl SpecSurface<'_> {
    /// The object as a whole, for "… must be an object".
    fn subject(self) -> String {
        match self {
            Self::JobSpec => "job spec".to_owned(),
            Self::DerivationSpec => "drv spec".to_owned(),
            Self::SugarOptions(helper) => format!("{helper}() options"),
        }
    }

    /// One member of the object, for "unknown …".
    fn member(self) -> String {
        match self {
            Self::JobSpec => "job spec field".to_owned(),
            Self::DerivationSpec => "drv spec field".to_owned(),
            Self::SugarOptions(helper) => format!("{helper}() option"),
        }
    }
}

/// Reject a float in a field that must be a whole number.
///
/// Without this, serde reports `invalid type: floating point 600.0, expected u64`
/// — which names neither the field nor anything the author can act on.
pub(super) fn reject_nonintegral_numbers(
    value: &Value,
    surface: SpecSurface<'_>,
    location: SourceLocation,
) -> Result<(), FlowError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    for field in NODE_SPEC_INTEGER_FIELDS {
        let Some(number) = object.get(*field) else {
            continue;
        };
        if !number.is_f64() {
            continue;
        }
        let seen = number.as_f64().unwrap_or_default();
        return Err(FlowError::new(
            "FlowSpecError",
            "invalid-spec",
            format!(
                "{} {field} must be a whole number, but arrived as the floating-point value \
                 {seen:?}; JavaScript arithmetic such as Math.floor() stays floating point even \
                 when its result is integral — coerce it with (x | 0)",
                surface.subject()
            ),
        )
        .at(location)
        .detail("field", (*field).to_owned())
        .detail("value", seen));
    }
    Ok(())
}

pub(super) fn reject_unknown_keys(
    value: &Value,
    surface: SpecSurface<'_>,
    allowed: &[&str],
    location: SourceLocation,
) -> Result<(), FlowError> {
    let object = value.as_object().ok_or_else(|| {
        FlowError::new(
            "FlowSpecError",
            "invalid-spec",
            format!("{} must be an object", surface.subject()),
        )
        .at(location)
    })?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(FlowError::new(
                "FlowSpecError",
                "unknown-spec-field",
                format!(
                    "unknown {} {key:?}, expected one of {}",
                    surface.member(),
                    allowed.join(", ")
                ),
            )
            .at(location)
            .detail("field", key.clone())
            .detail("expected", json!(allowed)));
        }
    }
    Ok(())
}

pub(super) fn required_string(
    value: Option<&JsValue>,
    label: &str,
    location: SourceLocation,
    context: &mut Context,
) -> JsResult<String> {
    let Some(value) = value else {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-argument",
                format!("{label} must be a string"),
            )
            .at(location),
            context,
        ));
    };
    let Some(value) = value.as_string() else {
        return Err(flow_to_js_error(
            FlowError::new(
                "FlowSpecError",
                "invalid-argument",
                format!("{label} must be a string"),
            )
            .at(location),
            context,
        ));
    };
    Ok(value.to_std_string_escaped())
}
