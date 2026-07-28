use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhFilterReason {
    SourceNotConfigured,
    SourceUnconstrained,
    RepositoryNotAllowed,
    ItemNotAllowlisted,
    LabelMismatch,
    StateMismatch,
    AssigneeMismatch,
    ItemKindMismatch,
    NotificationReasonMismatch,
    TriggerNotConfigured,
    SelfTriggerDisabled,
    TriggerActorNotAllowed,
    TriggerActorExcluded,
    TriggerActorUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmitOutcome {
    Emitted(PathBuf),
    Duplicate,
    Filtered { reason: GhFilterReason },
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GhDecisionStatus {
    Accepted,
    Filtered,
    Duplicate,
    Malformed,
    WouldEnqueue,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhCandidateSummary {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_actor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl GhCandidateSummary {
    pub(super) fn from_observation(observation: &GhObservation) -> Self {
        Self {
            source: observation.source.clone(),
            repo: Some(observation.repo.clone()),
            number: Some(observation.number),
            url: Some(observation.html_url.clone()),
            node_id: Some(observation.node_id.clone()),
            trigger_kind: Some(observation.trigger_kind.clone()),
            trigger_actor: Some(observation.trigger_actor.clone()),
            event_id: observation.event_id.clone(),
            comment_id: observation.comment_id.clone(),
            timestamp: Some(observation.trigger_timestamp.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhEnqueuePreview {
    pub task_uuid: String,
    pub argv: Vec<String>,
    #[serde(rename = "pool", serialize_with = "crate::poolset::serialize")]
    pub pools: Vec<String>,
    pub adapter: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_options: Option<AdapterJobOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_manifest: Option<GateManifestSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<String>,
    pub priority: Priority,
    pub dedup_key: String,
    pub context: GhOrigin,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhDecision {
    pub producer: String,
    pub candidate: GhCandidateSummary,
    pub decision: GhDecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<GhFilterReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_pointer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueue: Option<GhEnqueuePreview>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhTriggerAcknowledgement {
    pub schema_version: u32,
    pub producer: String,
    pub receipt_id: String,
    pub item_id: String,
    pub decision: GhDecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<GhFilterReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_uuid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_pointer: Option<String>,
}

pub trait GhAcknowledgementSink {
    fn post_acknowledgement(
        &mut self,
        acknowledgement: &GhTriggerAcknowledgement,
    ) -> Result<(), String>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct GhTriggerReceipt {
    pub(super) schema_version: u32,
    pub(super) receipt_id: String,
    pub(super) producer: String,
    pub(super) source: String,
    pub(super) item_id: String,
    pub(super) event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) comment_id: Option<String>,
    pub(super) trigger_kind: String,
    pub(super) trigger_actor: String,
    pub(super) trigger_timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) trigger_value: Option<String>,
    pub(super) primary_decision: GhDecisionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) rule: Option<GhFilterReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) task_uuid: Option<String>,
    pub(super) primary_acknowledged: bool,
    pub(super) duplicate_acknowledged: bool,
    pub(super) duplicate_count: u64,
}

pub(super) fn malformed_gh_decision(
    producer: &str,
    candidate: GhCandidateSummary,
    detail: String,
) -> GhDecision {
    GhDecision {
        producer: producer.to_owned(),
        candidate,
        decision: GhDecisionStatus::Malformed,
        rule: None,
        receipt_id: None,
        task_uuid: None,
        existing_task: None,
        status_pointer: None,
        enqueue: None,
        ingress: None,
        detail: Some(detail),
    }
}

pub(super) fn unavailable_actor_decision(
    producer: &str,
    source: String,
    node_id: String,
) -> GhDecision {
    GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary {
            source,
            repo: None,
            number: None,
            url: None,
            node_id: Some(node_id),
            trigger_kind: None,
            trigger_actor: None,
            event_id: None,
            comment_id: None,
            timestamp: None,
        },
        decision: GhDecisionStatus::Filtered,
        rule: Some(GhFilterReason::TriggerActorUnavailable),
        receipt_id: None,
        task_uuid: None,
        existing_task: None,
        status_pointer: None,
        enqueue: None,
        ingress: None,
        detail: None,
    }
}

pub(super) fn disabled_gh_decision(producer: &str, observation: &GhObservation) -> GhDecision {
    GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::Disabled,
        rule: None,
        receipt_id: None,
        task_uuid: None,
        existing_task: None,
        status_pointer: None,
        enqueue: None,
        ingress: None,
        detail: None,
    }
}

pub(super) fn filtered_gh_decision(
    producer: &str,
    observation: &GhObservation,
    receipt_id: String,
    rule: GhFilterReason,
) -> GhDecision {
    GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::Filtered,
        rule: Some(rule),
        receipt_id: Some(receipt_id),
        task_uuid: None,
        existing_task: None,
        status_pointer: None,
        enqueue: None,
        ingress: None,
        detail: None,
    }
}

pub(super) fn duplicate_gh_decision(
    producer: &str,
    observation: &GhObservation,
    receipt_id: &str,
    task_uuid: Option<String>,
) -> GhDecision {
    let status_pointer = task_uuid.as_ref().map(|task| status_pointer(task));
    GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::Duplicate,
        rule: None,
        receipt_id: Some(receipt_id.to_owned()),
        task_uuid: task_uuid.clone(),
        existing_task: task_uuid,
        status_pointer,
        enqueue: None,
        ingress: None,
        detail: None,
    }
}

pub(super) fn would_enqueue_gh_decision(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
    origin: GhOrigin,
    receipt_id: String,
    task_uuid: String,
    now: DateTime<Utc>,
) -> Result<GhDecision, ProducerError> {
    let enqueue = gh_enqueue_preview(config, origin, &task_uuid, now)?;
    Ok(GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::WouldEnqueue,
        rule: None,
        receipt_id: Some(receipt_id),
        task_uuid: Some(task_uuid.clone()),
        existing_task: None,
        status_pointer: Some(status_pointer(&task_uuid)),
        enqueue: Some(enqueue),
        ingress: None,
        detail: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn accepted_gh_decision(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
    origin: GhOrigin,
    receipt_id: String,
    task_uuid: String,
    ingress: Option<PathBuf>,
    now: DateTime<Utc>,
) -> Result<GhDecision, ProducerError> {
    let enqueue = gh_enqueue_preview(config, origin, &task_uuid, now)?;
    Ok(GhDecision {
        producer: producer.to_owned(),
        candidate: GhCandidateSummary::from_observation(observation),
        decision: GhDecisionStatus::Accepted,
        rule: None,
        receipt_id: Some(receipt_id),
        task_uuid: Some(task_uuid.clone()),
        existing_task: None,
        status_pointer: Some(status_pointer(&task_uuid)),
        enqueue: Some(enqueue),
        ingress,
        detail: None,
    })
}

pub(super) fn gh_enqueue_preview(
    config: &GhProducer,
    origin: GhOrigin,
    task_uuid: &str,
    now: DateTime<Utc>,
) -> Result<GhEnqueuePreview, ProducerError> {
    let payload = config.enqueue.payload(
        EnqueueSource::Gh,
        Some(&origin.producer),
        now,
        Some(&origin),
    )?;
    Ok(GhEnqueuePreview {
        task_uuid: task_uuid.to_owned(),
        argv: payload
            .argv
            .expect("producer enqueue payloads always contain direct argv"),
        pools: payload
            .pools
            .expect("producer enqueue payloads always contain pools"),
        adapter: payload
            .adapter
            .expect("producer enqueue payloads always contain an adapter"),
        cwd: payload.cwd,
        workspace: payload.workspace,
        adapter_options: payload.adapter_options,
        gate_manifest: payload.gate_manifest,
        executor: payload.executor,
        priority: payload
            .priority
            .expect("producer enqueue payloads always contain a priority"),
        dedup_key: gh_trigger_dedup_key(&origin)?,
        context: origin,
    })
}

pub(super) fn status_pointer(task_uuid: &str) -> String {
    format!("tally query log --task {task_uuid}")
}

pub(super) fn primary_receipt_decision(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
    receipt: &GhTriggerReceipt,
    now: DateTime<Utc>,
) -> Result<GhDecision, ProducerError> {
    match receipt.primary_decision {
        GhDecisionStatus::Accepted => {
            let task_uuid = receipt.task_uuid.clone().ok_or_else(|| {
                ProducerError::InvalidObservation(
                    "accepted GitHub trigger receipt omitted taskUuid".to_owned(),
                )
            })?;
            accepted_gh_decision(
                producer,
                config,
                observation,
                gh_origin(producer, config, observation),
                receipt.receipt_id.clone(),
                task_uuid,
                None,
                now,
            )
        }
        GhDecisionStatus::Filtered => Ok(filtered_gh_decision(
            producer,
            observation,
            receipt.receipt_id.clone(),
            receipt.rule.ok_or_else(|| {
                ProducerError::InvalidObservation(
                    "filtered GitHub trigger receipt omitted its rule".to_owned(),
                )
            })?,
        )),
        _ => Err(ProducerError::InvalidObservation(
            "GitHub trigger receipt has an invalid primary decision".to_owned(),
        )),
    }
}

pub(super) fn validate_receipt_identity(
    receipt: &GhTriggerReceipt,
    producer: &str,
    observation: &GhObservation,
    receipt_id: &str,
) -> Result<(), ProducerError> {
    if receipt.schema_version != 1
        || receipt.receipt_id != receipt_id
        || receipt.producer != producer
        || receipt.item_id != observation.node_id
        || receipt.comment_id != observation.comment_id
        || receipt.trigger_kind != observation.trigger_kind
        || receipt.trigger_actor != observation.trigger_actor
        || receipt.trigger_timestamp != observation.trigger_timestamp
        || receipt.trigger_value != observation.trigger_value
        || (matches!(observation.trigger_kind.as_str(), "assignment" | "label")
            && receipt.event_id != observation.event_id.as_deref().unwrap_or_default())
    {
        return Err(ProducerError::InvalidObservation(format!(
            "GitHub trigger receipt {receipt_id} does not match the observation"
        )));
    }
    Ok(())
}

pub(super) fn acknowledgement_for_decision(
    decision: &GhDecision,
    observation: &GhObservation,
) -> Result<GhTriggerAcknowledgement, ProducerError> {
    let receipt_id = decision.receipt_id.clone().ok_or_else(|| {
        ProducerError::InvalidObservation(
            "acknowledgeable GitHub decision omitted receiptId".to_owned(),
        )
    })?;
    if !matches!(
        decision.decision,
        GhDecisionStatus::Accepted | GhDecisionStatus::Filtered | GhDecisionStatus::Duplicate
    ) {
        return Err(ProducerError::InvalidObservation(
            "only accepted, filtered, and duplicate GitHub decisions are acknowledged".to_owned(),
        ));
    }
    Ok(GhTriggerAcknowledgement {
        schema_version: 1,
        producer: decision.producer.clone(),
        receipt_id,
        item_id: observation.node_id.clone(),
        decision: decision.decision,
        rule: decision.rule,
        task_uuid: decision.task_uuid.clone(),
        status_pointer: decision.status_pointer.clone(),
    })
}

pub(super) fn gh_origin(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
) -> GhOrigin {
    GhOrigin {
        schema_version: GH_ORIGIN_SCHEMA_VERSION,
        producer: producer.to_owned(),
        source: observation.source.clone(),
        repo: observation.repo.clone(),
        number: observation.number,
        html_url: observation.html_url.clone(),
        item_type: Some(observation.item_type),
        head_sha: observation.head_sha.clone(),
        node_id: observation.node_id.clone(),
        item_author: observation.item_author.clone(),
        trigger_actor: observation.trigger_actor.clone(),
        self_actor: observation.self_actor.clone(),
        notification_reason: observation.notification_reason.clone(),
        trigger_kind: observation.trigger_kind.clone(),
        event_id: observation.event_id.clone(),
        comment_id: observation.comment_id.clone(),
        trigger_timestamp: Some(observation.trigger_timestamp.clone()),
        trigger_value: observation.trigger_value.clone(),
        context: Some(observation.context.clone()),
        actor_exclude: config.actor_exclude.clone(),
        allow_self_triggered: config.allow_self_triggered,
        allowed_actors: config.allowed_actors.clone(),
    }
}

pub(super) fn gh_observation(origin: &GhOrigin) -> Result<GhObservation, ProducerError> {
    Ok(GhObservation {
        source: origin.source.clone(),
        repo: origin.repo.clone(),
        number: origin.number,
        html_url: origin.html_url.clone(),
        item_type: origin.item_type.ok_or_else(|| {
            ProducerError::InvalidObservation("GitHub origin omitted itemType".to_owned())
        })?,
        head_sha: origin.head_sha.clone(),
        node_id: origin.node_id.clone(),
        item_author: origin.item_author.clone(),
        trigger_actor: origin.trigger_actor.clone(),
        self_actor: origin.self_actor.clone(),
        notification_reason: origin.notification_reason.clone(),
        trigger_kind: origin.trigger_kind.clone(),
        event_id: origin.event_id.clone(),
        comment_id: origin.comment_id.clone(),
        trigger_timestamp: origin.trigger_timestamp.clone().ok_or_else(|| {
            ProducerError::InvalidObservation("GitHub origin omitted triggerTimestamp".to_owned())
        })?,
        trigger_value: origin.trigger_value.clone(),
        context: origin.context.clone().ok_or_else(|| {
            ProducerError::InvalidObservation("GitHub origin omitted context".to_owned())
        })?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct GhCompletionMarker {
    pub(super) completion_id: String,
    pub(super) producer: String,
    pub(super) source: String,
    pub(super) item_id: String,
}

pub(super) fn validate_gh_observation(
    producer: &str,
    config: &GhProducer,
    observation: &GhObservation,
) -> Result<(), ProducerError> {
    gh_origin(producer, config, observation)
        .validate()
        .map_err(|error| ProducerError::InvalidObservation(error.to_string()))?;
    Ok(())
}

pub(super) fn gh_filter_reason(
    config: &GhProducer,
    observation: &GhObservation,
) -> Option<GhFilterReason> {
    if let Some(reason) = gh_source_filter_reason(config, observation) {
        return Some(reason);
    }
    if !gh_trigger_matches(&config.triggers, observation) {
        return Some(GhFilterReason::TriggerNotConfigured);
    }
    if !config.allowed_actors.is_empty()
        && !config
            .allowed_actors
            .iter()
            .any(|actor| actor.eq_ignore_ascii_case(&observation.trigger_actor))
    {
        return Some(GhFilterReason::TriggerActorNotAllowed);
    }
    if observation.trigger_actor == observation.self_actor && !config.allow_self_triggered {
        return Some(GhFilterReason::SelfTriggerDisabled);
    }
    (config.actor_exclude != "self"
        && observation
            .trigger_actor
            .eq_ignore_ascii_case(&config.actor_exclude))
    .then_some(GhFilterReason::TriggerActorExcluded)
}

pub(super) fn gh_source_filter_reason(
    config: &GhProducer,
    observation: &GhObservation,
) -> Option<GhFilterReason> {
    let matching_kind = config
        .sources
        .iter()
        .filter(|source| source.kind() == observation.source)
        .collect::<Vec<_>>();
    if matching_kind.is_empty() {
        return Some(GhFilterReason::SourceNotConfigured);
    }
    let mut first_reason = None;
    for source in matching_kind {
        match gh_source_constraints_reason(source.constraints(), observation) {
            None => return None,
            Some(reason) if first_reason.is_none() => first_reason = Some(reason),
            Some(_) => {}
        }
    }
    first_reason
}

pub(super) fn gh_source_constraints_reason(
    constraints: &GhSourceConstraints,
    observation: &GhObservation,
) -> Option<GhFilterReason> {
    if !constraints.has_identity_scope() {
        return Some(GhFilterReason::SourceUnconstrained);
    }
    let explicit_repositories = constraints
        .repo
        .iter()
        .chain(constraints.repositories.iter());
    let repo_allowed = explicit_repositories
        .clone()
        .any(|repo| repo.eq_ignore_ascii_case(&observation.repo));
    let owner = observation.repo.split_once('/').map(|(owner, _)| owner);
    let owner_allowed = owner.is_some_and(|owner| {
        constraints
            .owners
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(owner))
    });
    let item_allowed = constraints
        .item_allowlist
        .iter()
        .any(|item| item == &observation.html_url);
    if !repo_allowed && !owner_allowed && !item_allowed {
        return Some(GhFilterReason::RepositoryNotAllowed);
    }
    if !constraints.item_allowlist.is_empty() && !item_allowed {
        return Some(GhFilterReason::ItemNotAllowlisted);
    }
    if !constraints.labels.iter().all(|required| {
        observation
            .context
            .labels
            .iter()
            .any(|actual| actual.eq_ignore_ascii_case(required))
    }) {
        return Some(GhFilterReason::LabelMismatch);
    }
    if constraints.state.is_some() && constraints.state != observation.context.state {
        return Some(GhFilterReason::StateMismatch);
    }
    if constraints.assignee.as_ref().is_some_and(|required| {
        !observation
            .context
            .assignees
            .iter()
            .any(|actual| actual.eq_ignore_ascii_case(required))
    }) {
        return Some(GhFilterReason::AssigneeMismatch);
    }
    if !constraints.kinds.is_empty()
        && !constraints
            .kinds
            .iter()
            .any(|kind| kind.matches(observation.item_type))
    {
        return Some(GhFilterReason::ItemKindMismatch);
    }
    if !constraints.notification_reasons.is_empty()
        && observation
            .notification_reason
            .as_ref()
            .is_none_or(|reason| {
                !constraints
                    .notification_reasons
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(reason))
            })
    {
        return Some(GhFilterReason::NotificationReasonMismatch);
    }
    None
}

pub(super) fn gh_trigger_matches(triggers: &GhTriggers, observation: &GhObservation) -> bool {
    match observation.trigger_kind.as_str() {
        "command-comment" => {
            observation
                .context
                .triggering_comment
                .as_ref()
                .is_some_and(|comment| {
                    triggers
                        .command_comments
                        .iter()
                        .any(|command| command == comment.body.trim())
                })
        }
        "mention" => observation
            .context
            .triggering_comment
            .as_ref()
            .is_some_and(|comment| {
                triggers
                    .mentions
                    .iter()
                    .any(|command| command == comment.body.trim())
            }),
        "assignment" => observation.trigger_value.as_ref().is_some_and(|assignee| {
            triggers
                .assignments
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(assignee))
        }),
        "label" => observation.trigger_value.as_ref().is_some_and(|label| {
            triggers
                .labels
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(label))
        }),
        _ => false,
    }
}
