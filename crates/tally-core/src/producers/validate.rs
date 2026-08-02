use super::*;

pub fn validate_registry(
    producers: &BTreeMap<String, ProducerConfig>,
    pools: &BTreeSet<String>,
    adapters: &BTreeSet<String>,
    executors: &BTreeSet<String>,
) -> Result<(), ProducerError> {
    let mut reachability_owners = BTreeMap::new();
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
                validate_enqueue(
                    name,
                    "enqueue",
                    &config.enqueue,
                    pools,
                    adapters,
                    executors,
                    false,
                )?;
            }
            ProducerConfig::EventsDir(config) => {
                if config.poll_interval_sec == 0 {
                    return Err(ProducerError::InvalidConfig(format!(
                        "events-dir producer {name:?} requires positive pollIntervalSec"
                    )));
                }
            }
            ProducerConfig::Gh(config) => {
                if config.poll_interval_sec == 0 {
                    return Err(ProducerError::InvalidConfig(format!(
                        "gh producer {name:?} requires positive pollIntervalSec"
                    )));
                }
                if config.enable && config.sources.is_empty() {
                    return Err(ProducerError::InvalidConfig(format!(
                        "enabled gh producer {name:?} requires at least one source"
                    )));
                }
                let mut sources = BTreeSet::new();
                for source in &config.sources {
                    let encoded = serde_json::to_string(source)?;
                    if !sources.insert(encoded) {
                        return Err(ProducerError::InvalidConfig(format!(
                            "gh producer {name:?} repeats source {source:?}"
                        )));
                    }
                    validate_gh_source(name, source)?;
                }
                validate_name(&config.actor_exclude, "GitHub actorExclude")?;
                let mut allowed_actors = BTreeSet::new();
                for actor in &config.allowed_actors {
                    validate_name(actor, "GitHub allowedActors entry")?;
                    if !allowed_actors.insert(actor) {
                        return Err(ProducerError::InvalidConfig(format!(
                            "gh producer {name:?} repeats allowedActors entry {actor:?}"
                        )));
                    }
                }
                validate_gh_triggers(name, &config.triggers)?;
                let mut reviewers = BTreeSet::new();
                for reviewer in &config.reviewers {
                    validate_gh_login(name, reviewer)?;
                    if !reviewers.insert(reviewer) {
                        return Err(ProducerError::InvalidConfig(format!(
                            "gh producer {name:?} repeats reviewers entry {reviewer:?}"
                        )));
                    }
                }
                if config.request_review && config.reviewers.is_empty() {
                    return Err(ProducerError::InvalidConfig(format!(
                        "gh producer {name:?} requestReview=true requires a non-empty reviewers list"
                    )));
                }
                if config.close_on_pass == Some(true) && !config.post_evidence {
                    return Err(ProducerError::InvalidConfig(format!(
                        "gh producer {name:?} closeOnPass=true requires postEvidence=true"
                    )));
                }
                if config.post_failure_stderr && !config.post_failure_evidence {
                    return Err(ProducerError::InvalidConfig(format!(
                        "gh producer {name:?} postFailureStderr=true requires postFailureEvidence=true"
                    )));
                }
                if (config.post_gate_summary || config.close_on_acceptance)
                    && config.enqueue.gate_manifest.is_none()
                {
                    return Err(ProducerError::InvalidConfig(format!(
                        "gh producer {name:?} postGateSummary/closeOnAcceptance requires enqueue.gateManifest"
                    )));
                }
                validate_enqueue(
                    name,
                    "enqueue",
                    &config.enqueue,
                    pools,
                    adapters,
                    executors,
                    true,
                )?;
            }
            ProducerConfig::BuildEffect(config) => {
                if !config.path.is_absolute() {
                    return Err(ProducerError::InvalidConfig(format!(
                        "build-effect producer {name:?} path must be absolute"
                    )));
                }
                validate_safe_path(
                    &config.path,
                    &format!("build-effect producer {name:?} path"),
                )?;
                validate_enqueue(
                    name,
                    "onKey",
                    &config.on_key,
                    pools,
                    adapters,
                    executors,
                    false,
                )?;
            }
            ProducerConfig::PoolReachability(config) => {
                if config.interval_sec == 0 || config.hysteresis == 0 {
                    return Err(ProducerError::InvalidConfig(format!(
                        "pool-reachability producer {name:?} requires positive intervalSec and hysteresis"
                    )));
                }
                if !pools.contains(&config.probe_pool) {
                    return Err(ProducerError::InvalidConfig(format!(
                        "pool-reachability producer {name:?} references unknown probePool {:?}",
                        config.probe_pool
                    )));
                }
                if let Some(existing) =
                    reachability_owners.insert(config.probe_pool.clone(), name.clone())
                {
                    return Err(ProducerError::InvalidConfig(format!(
                        "pool-reachability producers {existing:?} and {name:?} both own probePool {:?}",
                        config.probe_pool
                    )));
                }
                for (field, enqueue) in [
                    ("onLost", config.on_lost.as_ref()),
                    ("onReturn", config.on_return.as_ref()),
                    ("onReturnAttest", config.on_return_attest.as_ref()),
                ] {
                    if let Some(enqueue) = enqueue {
                        validate_enqueue(name, field, enqueue, pools, adapters, executors, false)?;
                    }
                }
                if config
                    .on_return_attest
                    .as_ref()
                    .is_some_and(|enqueue| !enqueue.no_enqueue)
                {
                    return Err(ProducerError::InvalidConfig(format!(
                        "pool-reachability producer {name:?} onReturnAttest requires noEnqueue=true"
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
    allow_origin_templates: bool,
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
        validate_origin_template(argument, allow_origin_templates).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} argv template is invalid: {detail}"
            ))
        })?;
    }
    if let Some(brief) = &enqueue.brief {
        validate_origin_value(brief, allow_origin_templates).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} brief template is invalid: {detail}"
            ))
        })?;
    }
    if let Some(cwd) = &enqueue.cwd {
        let cwd = cwd.to_str().ok_or_else(|| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} cwd must be valid UTF-8"
            ))
        })?;
        validate_origin_template(cwd, allow_origin_templates).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} cwd template is invalid: {detail}"
            ))
        })?;
        validate_resolved_path_template(cwd).map_err(|detail| {
            ProducerError::InvalidConfig(format!(
                "producer {producer:?} {field} cwd is invalid: {detail}"
            ))
        })?;
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

pub(super) const ORIGIN_TEMPLATE_FIELDS: &[&str] = &[
    "repoName",
    "gh.source",
    "gh.repo",
    "gh.repoName",
    "gh.number",
    "gh.url",
    "gh.type",
    "gh.headSha",
    "gh.nodeId",
    "gh.itemAuthor",
    "gh.triggerActor",
    "gh.selfActor",
    "gh.notificationReason",
    "gh.triggerKind",
    "gh.eventId",
    "gh.commentId",
    "gh.triggerTimestamp",
    "gh.triggerValue",
];

pub(super) fn validate_origin_template(template: &str, allowed: bool) -> Result<(), String> {
    if template.contains('\0') {
        return Err("template contains a NUL byte".to_owned());
    }
    let fields = origin_template_fields(template)?;
    if !allowed && !fields.is_empty() {
        return Err(
            "GitHub origin placeholders are valid only in a gh producer enqueue".to_owned(),
        );
    }
    for field in fields {
        if !ORIGIN_TEMPLATE_FIELDS.contains(&field) {
            return Err(format!("unknown placeholder {field:?}"));
        }
    }
    Ok(())
}

pub(super) fn origin_template_fields(mut template: &str) -> Result<Vec<&str>, String> {
    let mut fields = Vec::new();
    while let Some(start) = template.find("${") {
        template = &template[start + 2..];
        let end = template
            .find('}')
            .ok_or_else(|| "unclosed '${field}' placeholder".to_owned())?;
        let field = &template[..end];
        if field.is_empty()
            || !field
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_'))
        {
            return Err(format!("invalid placeholder name {field:?}"));
        }
        fields.push(field);
        template = &template[end + 1..];
    }
    Ok(fields)
}

pub(super) fn render_origin_template(
    template: &str,
    origin: Option<&GhOrigin>,
) -> Result<String, ProducerError> {
    render_origin_template_with_limit(template, origin, Some(64 * 1024))
}

fn render_origin_template_with_limit(
    template: &str,
    origin: Option<&GhOrigin>,
    max_bytes: Option<usize>,
) -> Result<String, ProducerError> {
    validate_origin_template(template, origin.is_some())
        .map_err(ProducerError::InvalidObservation)?;
    let mut rendered = String::new();
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        rendered.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let end = rest
            .find('}')
            .expect("validated origin templates have closing braces");
        let field = &rest[..end];
        let origin = origin.ok_or_else(|| {
            ProducerError::InvalidObservation(
                "GitHub origin placeholder has no GitHub observation".to_owned(),
            )
        })?;
        rendered.push_str(origin_template_value(origin, field)?.as_str());
        rest = &rest[end + 1..];
    }
    rendered.push_str(rest);
    if max_bytes.is_some_and(|max_bytes| rendered.len() > max_bytes) {
        return Err(ProducerError::InvalidObservation(
            "rendered origin template exceeds 65536 bytes".to_owned(),
        ));
    }
    Ok(rendered)
}

pub(super) fn validate_origin_value(value: &Value, allowed: bool) -> Result<(), String> {
    match value {
        Value::String(value) => validate_origin_template(value, allowed),
        Value::Array(values) => values
            .iter()
            .try_for_each(|value| validate_origin_value(value, allowed)),
        Value::Object(values) => values.iter().try_for_each(|(name, value)| {
            validate_origin_template(name, allowed)?;
            validate_origin_value(value, allowed)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
    }
}

pub(super) fn render_origin_value(
    value: &Value,
    origin: Option<&GhOrigin>,
) -> Result<Value, ProducerError> {
    match value {
        Value::String(value) => {
            render_origin_template_with_limit(value, origin, None).map(Value::String)
        }
        Value::Array(values) => values
            .iter()
            .map(|value| render_origin_value(value, origin))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(values) => {
            let mut rendered = serde_json::Map::new();
            for (name, value) in values {
                let name = render_origin_template_with_limit(name, origin, None)?;
                let value = render_origin_value(value, origin)?;
                if rendered.insert(name.clone(), value).is_some() {
                    return Err(ProducerError::InvalidObservation(format!(
                        "rendered producer brief contains duplicate field {name:?}"
                    )));
                }
            }
            Ok(Value::Object(rendered))
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(value.clone()),
    }
}

pub(super) fn origin_template_value(
    origin: &GhOrigin,
    field: &str,
) -> Result<String, ProducerError> {
    let value = match field {
        "gh.source" => Some(origin.source.clone()),
        "gh.repo" => Some(origin.repo.clone()),
        "repoName" | "gh.repoName" => origin
            .repo
            .rsplit_once('/')
            .map(|(_, name)| name.to_owned()),
        "gh.number" => Some(origin.number.to_string()),
        "gh.url" => Some(origin.html_url.clone()),
        "gh.type" => origin.item_type.map(|kind| kind.as_str().to_owned()),
        "gh.headSha" => origin.head_sha.clone(),
        "gh.nodeId" => Some(origin.node_id.clone()),
        "gh.itemAuthor" => Some(origin.item_author.clone()),
        "gh.triggerActor" => Some(origin.trigger_actor.clone()),
        "gh.selfActor" => Some(origin.self_actor.clone()),
        "gh.notificationReason" => origin.notification_reason.clone(),
        "gh.triggerKind" => Some(origin.trigger_kind.clone()),
        "gh.eventId" => origin.event_id.clone(),
        "gh.commentId" => origin.comment_id.clone(),
        "gh.triggerTimestamp" => origin.trigger_timestamp.clone(),
        "gh.triggerValue" => origin.trigger_value.clone(),
        _ => None,
    };
    value.ok_or_else(|| {
        ProducerError::InvalidObservation(format!(
            "origin field {field:?} is absent for this GitHub item"
        ))
    })
}

pub(super) fn validate_resolved_path_template(path: &str) -> Result<(), String> {
    if !path.starts_with('/')
        || path.contains('%')
        || path.contains('\0')
        || path.chars().any(char::is_control)
    {
        return Err(
            "path must be absolute and contain no control characters or systemd specifiers"
                .to_owned(),
        );
    }
    Ok(())
}

pub(super) fn validate_resolved_path(path: &Path, label: &str) -> Result<(), ProducerError> {
    let path = path
        .to_str()
        .ok_or_else(|| ProducerError::InvalidObservation(format!("{label} must be valid UTF-8")))?;
    validate_resolved_path_template(path)
        .map_err(|detail| ProducerError::InvalidObservation(format!("{label}: {detail}")))
}

pub(super) fn validate_name(value: &str, label: &str) -> Result<(), ProducerError> {
    if value.trim().is_empty()
        || value.len() > MAX_GH_ORIGIN_FIELD_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ProducerError::InvalidConfig(format!(
            "{label} must be non-empty, at most {MAX_GH_ORIGIN_FIELD_BYTES} bytes, and contain no control characters"
        )));
    }
    Ok(())
}

/// A reviewer login is rendered straight into an `@mention` on a public issue
/// and into a GraphQL user lookup, so it is held to GitHub's own login grammar
/// rather than the permissive field bound: alphanumerics and interior hyphens,
/// at most 39 characters. Nothing that could carry markdown, a second mention,
/// or a query fragment gets past configuration validation.
pub(super) fn validate_gh_login(producer: &str, login: &str) -> Result<(), ProducerError> {
    let valid = !login.is_empty()
        && login.len() <= 39
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !login.starts_with('-')
        && !login.ends_with('-')
        && !login.contains("--");
    if !valid {
        return Err(ProducerError::InvalidConfig(format!(
            "gh producer {producer:?} reviewers entry {login:?} is not a GitHub login"
        )));
    }
    Ok(())
}

pub(super) fn validate_gh_source(producer: &str, source: &GhSource) -> Result<(), ProducerError> {
    let constraints = source.constraints();
    let mut repositories = constraints.repositories.clone();
    if let Some(repo) = &constraints.repo {
        repositories.push(repo.clone());
    }
    validate_unique_values(
        &repositories,
        &format!("gh producer {producer:?} {} repositories", source.kind()),
    )?;
    for repo in &repositories {
        validate_repo_constraint(repo)?;
    }
    for (label, values) in [
        ("owners", &constraints.owners),
        ("labels", &constraints.labels),
        ("notificationReasons", &constraints.notification_reasons),
        ("itemAllowlist", &constraints.item_allowlist),
    ] {
        validate_unique_values(
            values,
            &format!("gh producer {producer:?} {} {label}", source.kind()),
        )?;
    }
    for owner in &constraints.owners {
        validate_login(owner, "GitHub source owner")?;
    }
    if let Some(assignee) = &constraints.assignee {
        validate_login(assignee, "GitHub source assignee")?;
    }
    for item in &constraints.item_allowlist {
        parse_gh_item_url(item).map_err(|reason| {
            ProducerError::InvalidConfig(format!(
                "gh producer {producer:?} {} itemAllowlist entry {item:?} is invalid: {reason}",
                source.kind()
            ))
        })?;
    }
    if let Some(query) = &constraints.query {
        validate_name(query, "GitHub raw query")?;
        if !matches!(source, GhSource::Search(_)) {
            return Err(ProducerError::InvalidConfig(format!(
                "gh producer {producer:?} notification source cannot carry query"
            )));
        }
    }
    if !constraints.notification_reasons.is_empty() && !matches!(source, GhSource::Notifications(_))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "gh producer {producer:?} search source cannot carry notificationReasons"
        )));
    }
    Ok(())
}

pub(super) fn validate_gh_triggers(
    producer: &str,
    triggers: &GhTriggers,
) -> Result<(), ProducerError> {
    for (label, values) in [
        ("commandComments", &triggers.command_comments),
        ("mentions", &triggers.mentions),
        ("assignments", &triggers.assignments),
        ("labels", &triggers.labels),
    ] {
        validate_unique_values(
            values,
            &format!("gh producer {producer:?} triggers.{label}"),
        )?;
    }
    for command in &triggers.command_comments {
        if !valid_explicit_comment_command(command, '/') {
            return Err(ProducerError::InvalidConfig(format!(
                "gh producer {producer:?} command comment {command:?} is not an explicit slash-command grammar"
            )));
        }
    }
    for mention in &triggers.mentions {
        if !valid_explicit_comment_command(mention, '@') {
            return Err(ProducerError::InvalidConfig(format!(
                "gh producer {producer:?} mention {mention:?} is not an explicit mention-command grammar"
            )));
        }
    }
    for actor in &triggers.assignments {
        validate_login(actor, "GitHub assignment trigger")?;
    }
    Ok(())
}

pub(super) fn validate_unique_values(values: &[String], label: &str) -> Result<(), ProducerError> {
    let mut unique = BTreeSet::new();
    for value in values {
        validate_name(value, label)?;
        if !unique.insert(value) {
            return Err(ProducerError::InvalidConfig(format!(
                "{label} contains duplicate value {value:?}"
            )));
        }
    }
    Ok(())
}

pub(super) fn validate_repo_constraint(repo: &str) -> Result<(), ProducerError> {
    let Some((owner, name)) = repo.split_once('/') else {
        return Err(ProducerError::InvalidConfig(format!(
            "GitHub repository {repo:?} must be owner/name"
        )));
    };
    validate_login(owner, "GitHub repository owner")?;
    if name.is_empty()
        || name.contains('/')
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "GitHub repository {repo:?} must be a safe owner/name pair"
        )));
    }
    Ok(())
}

pub(super) fn validate_login(login: &str, label: &str) -> Result<(), ProducerError> {
    validate_name(login, label)?;
    if !login
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(ProducerError::InvalidConfig(format!(
            "{label} {login:?} is not a safe GitHub login"
        )));
    }
    Ok(())
}

pub(super) fn valid_explicit_comment_command(command: &str, prefix: char) -> bool {
    if command.len() > 128
        || command.trim() != command
        || command.chars().any(char::is_control)
        || !command.starts_with(prefix)
    {
        return false;
    }
    let mut tokens = command.split(' ');
    let Some(first) = tokens.next() else {
        return false;
    };
    let valid_token = |token: &str| {
        !token.is_empty()
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    };
    valid_token(&first[1..]) && tokens.all(valid_token)
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
