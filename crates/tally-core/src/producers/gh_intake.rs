use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhObservation {
    pub source: String,
    pub repo: String,
    pub number: u64,
    pub html_url: String,
    pub item_type: GhItemType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub node_id: String,
    pub item_author: String,
    pub trigger_actor: String,
    pub self_actor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_reason: Option<String>,
    pub trigger_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment_id: Option<String>,
    pub trigger_timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_value: Option<String>,
    pub context: GhContextSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhObservationInput {
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub html_url: Option<String>,
    #[serde(default)]
    pub item_type: Option<GhItemType>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default, alias = "itemId")]
    pub node_id: Option<String>,
    #[serde(default)]
    pub item_author: Option<String>,
    #[serde(default)]
    pub trigger_actor: Option<String>,
    #[serde(default)]
    pub self_actor: Option<String>,
    #[serde(default)]
    pub notification_reason: Option<String>,
    #[serde(default)]
    pub trigger_kind: Option<String>,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub comment_id: Option<String>,
    #[serde(default)]
    pub trigger_timestamp: Option<String>,
    #[serde(default)]
    pub trigger_value: Option<String>,
    #[serde(default)]
    pub context: Option<GhContextSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ProducerObservation {
    Calendar,
    EventsDir,
    Gh(Box<GhObservationInput>),
    BuildEffect {
        #[serde(default)]
        store_path: Option<PathBuf>,
    },
    PoolReachability {
        reachable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AssistedBy {
    pub adapter: String,
    pub model: String,
    pub task_uuid: String,
    pub witness_seq: u64,
}

impl AssistedBy {
    fn trailer(&self) -> String {
        format!(
            "Assisted-by: {}:{} (tally:{} witness:{})",
            self.adapter, self.model, self.task_uuid, self.witness_seq
        )
    }
}

pub(super) fn assisted_by_from_evidence(evidence: &Value) -> Option<AssistedBy> {
    let adapter = evidence.get("adapter")?.as_str()?;
    let model = evidence.get("model")?.as_str()?;
    let task_uuid = evidence.get("taskUuid")?.as_str()?;
    let witness_seq = evidence
        .get("witnessSeq")?
        .as_u64()
        .filter(|seq| *seq > 0)?;
    if adapter.is_empty()
        || model.is_empty()
        || adapter.chars().any(char::is_control)
        || model.chars().any(char::is_control)
        || Uuid::parse_str(task_uuid).is_err()
    {
        return None;
    }
    Some(AssistedBy {
        adapter: adapter.to_owned(),
        model: model.to_owned(),
        task_uuid: task_uuid.to_owned(),
        witness_seq,
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GhCompletedMutation {
    pub producer: String,
    pub source: String,
    pub item_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_id: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_summary: Option<GateSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acceptance: Option<AcceptanceFact>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub request_review: bool,
    #[serde(skip)]
    pub assisted_by: Option<AssistedBy>,
}

pub trait GhMutationSink {
    fn post_evidence(&mut self, mutation: &GhCompletedMutation) -> Result<(), String>;
    fn close_item(&mut self, mutation: &GhCompletedMutation) -> Result<(), String>;
}

pub(super) const GH_COMPLETION_STATE_GRAPHQL: &str = r#"query TallyCompletionState($itemId: ID!, $cursor: String) {
  node(id: $itemId) {
    __typename
    ... on Issue { state comments(first: 100, after: $cursor) { nodes { body } pageInfo { hasNextPage endCursor } } }
    ... on PullRequest { state comments(first: 100, after: $cursor) { nodes { body } pageInfo { hasNextPage endCursor } } }
  }
}"#;
pub(super) const GH_COMPLETION_COMMENT_GRAPHQL: &str = r#"mutation TallyCompletionComment($itemId: ID!, $body: String!) {
  addComment(input: {subjectId: $itemId, body: $body}) { commentEdge { node { id } } }
}"#;
pub(super) const GH_COMPLETION_ISSUE_GRAPHQL: &str = r#"mutation TallyCompletionIssue($itemId: ID!) {
  closeIssue(input: {issueId: $itemId, stateReason: COMPLETED}) { issue { id state stateReason } }
}"#;
pub(super) const GH_COMPLETION_PULL_REQUEST_GRAPHQL: &str = r#"mutation TallyCompletionPullRequest($itemId: ID!) {
  closePullRequest(input: {pullRequestId: $itemId}) { pullRequest { id state } }
}"#;
pub(super) const GH_PROCESS_TIMEOUT: Duration = Duration::from_secs(20);
pub(super) const GH_READER_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);
pub(super) const MAX_GH_PROCESS_OUTPUT_BYTES: usize = 1024 * 1024;
pub(super) const MAX_GH_COMMENT_PAGES: usize = 100;

#[derive(Debug, Clone)]
pub struct GhCliMutationSink {
    program: PathBuf,
}

#[derive(Debug, Clone)]
pub struct GhCliIntake {
    program: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct GhCliAcknowledgementSink {
    mutation: GhCliMutationSink,
}

impl Default for GhCliMutationSink {
    fn default() -> Self {
        Self {
            program: PathBuf::from("gh"),
        }
    }
}

impl GhCliMutationSink {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl Default for GhCliIntake {
    fn default() -> Self {
        Self {
            program: PathBuf::from("gh"),
        }
    }
}

impl GhCliAcknowledgementSink {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            mutation: GhCliMutationSink::with_program(program),
        }
    }
}

impl GhCliIntake {
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    pub(super) fn poll(
        &self,
        config: &GhProducer,
    ) -> Result<Vec<GhIntakeCandidate>, ProducerError> {
        let viewer: Value = self.json(&["api", "user"])?;
        let self_actor = viewer
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| !login.is_empty())
            .ok_or_else(|| {
                ProducerError::GitHub("gh api user omitted a non-empty login".to_owned())
            })?
            .to_owned();
        let mut observations = Vec::new();
        for source in &config.sources {
            let constraints = source.constraints();
            if !constraints.has_identity_scope() {
                continue;
            }
            match source.kind() {
                "notifications" => {
                    let notifications = self.paginated_notifications()?;
                    for notification in &notifications {
                        let subject = notification.get("subject").ok_or_else(|| {
                            ProducerError::GitHub("GitHub notification omitted subject".to_owned())
                        })?;
                        let kind =
                            subject.get("type").and_then(Value::as_str).ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub notification subject omitted type".to_owned(),
                                )
                            })?;
                        let item_type = match kind {
                            "Issue" => GhItemType::Issue,
                            "PullRequest" => GhItemType::PullRequest,
                            _ => continue,
                        };
                        let url = subject.get("url").and_then(Value::as_str).ok_or_else(|| {
                            ProducerError::GitHub(
                                "GitHub issue/PR notification omitted subject URL".to_owned(),
                            )
                        })?;
                        let endpoint_offset = url.find("/repos/").ok_or_else(|| {
                            ProducerError::GitHub(format!(
                                "GitHub notification subject URL {url:?} is not a repository issue/PR endpoint"
                            ))
                        })?;
                        let hydrated: Value = self.json(&["api", &url[endpoint_offset..]])?;
                        let triggering_comment = match subject
                            .get("latest_comment_url")
                            .and_then(Value::as_str)
                            .filter(|url| !url.is_empty())
                        {
                            Some(url) => {
                                let offset = url.find("/repos/").ok_or_else(|| {
                                    ProducerError::GitHub(format!(
                                        "GitHub latest comment URL {url:?} is not a repository comment endpoint"
                                    ))
                                })?;
                                let comment = self.json(&["api", &url[offset..]])?;
                                exact_notification_comment(notification, url, &comment)
                            }
                            None => None,
                        };
                        let repo = notification
                            .pointer("/repository/full_name")
                            .and_then(Value::as_str);
                        let notification_timestamp = notification
                            .get("updated_at")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub notification omitted updated_at".to_owned(),
                                )
                            })?;
                        let event_trigger = if triggering_comment.is_none() {
                            let repo = repo.ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub notification omitted repository identity".to_owned(),
                                )
                            })?;
                            let number = hydrated
                                .get("number")
                                .and_then(Value::as_u64)
                                .filter(|number| *number > 0)
                                .ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub notification item omitted number".to_owned(),
                                    )
                                })?;
                            self.notification_event_trigger(
                                repo,
                                number,
                                notification_timestamp,
                                &config.triggers,
                            )?
                        } else {
                            None
                        };
                        let event_id = event_trigger
                            .as_ref()
                            .map(|(event, _)| event.id.clone())
                            .or_else(|| notification.get("id").and_then(json_identifier));
                        let event_timestamp = event_trigger
                            .as_ref()
                            .map(|(_, timestamp)| timestamp.clone())
                            .unwrap_or_else(|| notification_timestamp.to_owned());
                        let event_trigger = event_trigger.map(|(event, _)| event);
                        let reason = notification
                            .get("reason")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                        observations.push(gh_api_candidate(
                            "notifications",
                            &hydrated,
                            &self_actor,
                            GhObservationHints {
                                repo,
                                item_type: Some(item_type),
                                notification_reason: reason,
                                event_id,
                                triggering_comment,
                                event_trigger,
                                trigger_timestamp: Some(&event_timestamp),
                                triggers: &config.triggers,
                            },
                        )?);
                    }
                }
                "search" => {
                    for query in gh_search_queries(constraints) {
                        let query_field = format!("q={query}");
                        let response: Value = self.json(&[
                            "api",
                            "--method",
                            "GET",
                            "search/issues",
                            "-f",
                            &query_field,
                            "-f",
                            "per_page=100",
                        ])?;
                        let items =
                            response
                                .get("items")
                                .and_then(Value::as_array)
                                .ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub search response omitted items array".to_owned(),
                                    )
                                })?;
                        for item in items {
                            let item_type = if item.get("pull_request").is_some() {
                                GhItemType::PullRequest
                            } else {
                                GhItemType::Issue
                            };
                            let hydrated = if item_type == GhItemType::PullRequest {
                                let url = item
                                    .pointer("/pull_request/url")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        ProducerError::GitHub(
                                            "GitHub PR search result omitted pull_request.url"
                                                .to_owned(),
                                        )
                                    })?;
                                let endpoint_offset = url.find("/repos/").ok_or_else(|| {
                                    ProducerError::GitHub(format!(
                                        "GitHub PR search URL {url:?} is not a repository endpoint"
                                    ))
                                })?;
                                self.json(&["api", &url[endpoint_offset..]])?
                            } else {
                                item.clone()
                            };
                            let repo = item
                                .get("repository_url")
                                .and_then(Value::as_str)
                                .and_then(repo_from_api_url)
                                .ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub search result omitted repository identity"
                                            .to_owned(),
                                    )
                                })?;
                            let number = hydrated
                                .get("number")
                                .and_then(Value::as_u64)
                                .filter(|number| *number > 0)
                                .ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub search result omitted item number".to_owned(),
                                    )
                                })?;
                            let comments_endpoint =
                                format!("/repos/{repo}/issues/{number}/comments?per_page=100");
                            let comments = self.json(&["api", &comments_endpoint])?;
                            let comments = comments.as_array().ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub issue comments response must be an array".to_owned(),
                                )
                            })?;
                            for comment in comments {
                                let Some(triggering_comment) = gh_triggering_comment(comment)
                                else {
                                    continue;
                                };
                                if !comment_is_configured_trigger(
                                    &config.triggers,
                                    &triggering_comment.body,
                                ) {
                                    continue;
                                }
                                let timestamp = gh_comment_timestamp(comment).ok_or_else(|| {
                                    ProducerError::GitHub(
                                        "GitHub trigger comment omitted updated_at and created_at"
                                            .to_owned(),
                                    )
                                })?;
                                observations.push(gh_api_candidate(
                                    "search",
                                    &hydrated,
                                    &self_actor,
                                    GhObservationHints {
                                        repo: Some(repo),
                                        item_type: Some(item_type),
                                        notification_reason: None,
                                        event_id: Some(triggering_comment.id.clone()),
                                        triggering_comment: Some(triggering_comment),
                                        event_trigger: None,
                                        trigger_timestamp: Some(timestamp),
                                        triggers: &config.triggers,
                                    },
                                )?);
                            }
                            let events_endpoint =
                                format!("/repos/{repo}/issues/{number}/events?per_page=100");
                            let events = self.json(&["api", &events_endpoint])?;
                            let events = events.as_array().ok_or_else(|| {
                                ProducerError::GitHub(
                                    "GitHub issue events response must be an array".to_owned(),
                                )
                            })?;
                            for event in events {
                                let Some(event_trigger) =
                                    configured_gh_event(event, &config.triggers)
                                else {
                                    continue;
                                };
                                let timestamp = event
                                    .get("created_at")
                                    .and_then(Value::as_str)
                                    .ok_or_else(|| {
                                        ProducerError::GitHub(
                                            "GitHub trigger event omitted created_at".to_owned(),
                                        )
                                    })?;
                                let event_id = event_trigger.id.clone();
                                observations.push(gh_api_candidate(
                                    "search",
                                    &hydrated,
                                    &self_actor,
                                    GhObservationHints {
                                        repo: Some(repo),
                                        item_type: Some(item_type),
                                        notification_reason: None,
                                        event_id: Some(event_id),
                                        triggering_comment: None,
                                        event_trigger: Some(event_trigger),
                                        trigger_timestamp: Some(timestamp),
                                        triggers: &config.triggers,
                                    },
                                )?);
                            }
                        }
                    }
                }
                other => {
                    return Err(ProducerError::InvalidConfig(format!(
                        "unsupported GitHub source {other:?}"
                    )))
                }
            }
        }
        normalize_gh_candidates(config, &mut observations);
        Ok(observations)
    }

    fn paginated_notifications(&self) -> Result<Vec<Value>, ProducerError> {
        let mut collected = Vec::new();
        for page in 1..=MAX_GH_COMMENT_PAGES {
            let page_field = format!("page={page}");
            let mut args = vec![
                "api",
                "--method",
                "GET",
                "notifications",
                "-f",
                "all=false",
                "-f",
                "participating=false",
                "-f",
                "per_page=100",
            ];
            if page > 1 {
                args.extend(["-f", page_field.as_str()]);
            }
            let response = self.json(&args)?;
            let Value::Array(notifications) = response else {
                return Err(ProducerError::GitHub(
                    "gh notifications response must be an array".to_owned(),
                ));
            };
            let complete = notifications.len() < 100;
            collected.extend(notifications);
            if complete {
                return Ok(collected);
            }
        }
        Err(ProducerError::GitHub(format!(
            "GitHub notifications truncated at the \
             {MAX_GH_COMMENT_PAGES}-page intake cap"
        )))
    }

    fn json(&self, args: &[&str]) -> Result<Value, ProducerError> {
        let output = run_gh_bounded(&self.program, args, None).map_err(ProducerError::GitHub)?;
        serde_json::from_slice(&output).map_err(|error| {
            ProducerError::GitHub(format!(
                "{} returned invalid JSON: {error}",
                self.program.display()
            ))
        })
    }

    fn notification_event_trigger(
        &self,
        repo: &str,
        number: u64,
        notification_timestamp: &str,
        triggers: &GhTriggers,
    ) -> Result<Option<(GhEventTrigger, String)>, ProducerError> {
        let endpoint = format!("/repos/{repo}/issues/{number}/events?per_page=100");
        let events = self.json(&["api", &endpoint])?;
        let events = events.as_array().ok_or_else(|| {
            ProducerError::GitHub("GitHub issue events response must be an array".to_owned())
        })?;
        for event in events.iter().rev() {
            let Some(timestamp) = event.get("created_at").and_then(Value::as_str) else {
                continue;
            };
            if !gh_timestamps_equal(notification_timestamp, timestamp) {
                continue;
            }
            if let Some(event) = configured_gh_event(event, triggers) {
                return Ok(Some((event, timestamp.to_owned())));
            }
        }
        Ok(None)
    }

    pub(super) fn item(
        &self,
        config: &GhProducer,
        item_url: &str,
    ) -> Result<Vec<GhIntakeCandidate>, ProducerError> {
        let location = parse_gh_item_url(item_url).map_err(ProducerError::InvalidObservation)?;
        let viewer: Value = self.json(&["api", "user"])?;
        let self_actor = viewer
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| !login.is_empty())
            .ok_or_else(|| {
                ProducerError::GitHub("gh api user omitted a non-empty login".to_owned())
            })?;
        let item_endpoint = match location.item_type {
            GhItemType::Issue => {
                format!("/repos/{}/issues/{}", location.repo, location.number)
            }
            GhItemType::PullRequest => {
                format!("/repos/{}/pulls/{}", location.repo, location.number)
            }
        };
        let item = self.json(&["api", &item_endpoint])?;
        let comments_endpoint = format!(
            "/repos/{}/issues/{}/comments?per_page=100",
            location.repo, location.number
        );
        let comments = self.json(&["api", &comments_endpoint])?;
        let comments = comments.as_array().ok_or_else(|| {
            ProducerError::GitHub("GitHub issue comments response must be an array".to_owned())
        })?;
        let mut candidates = Vec::new();
        for comment in comments {
            let Some(triggering_comment) = gh_triggering_comment(comment) else {
                continue;
            };
            if !comment_is_configured_trigger(&config.triggers, &triggering_comment.body) {
                continue;
            }
            let timestamp = gh_comment_timestamp(comment).ok_or_else(|| {
                ProducerError::GitHub(
                    "GitHub trigger comment omitted updated_at and created_at".to_owned(),
                )
            })?;
            candidates.push(gh_api_candidate(
                "search",
                &item,
                self_actor,
                GhObservationHints {
                    repo: Some(&location.repo),
                    item_type: Some(location.item_type),
                    notification_reason: None,
                    event_id: Some(triggering_comment.id.clone()),
                    triggering_comment: Some(triggering_comment),
                    event_trigger: None,
                    trigger_timestamp: Some(timestamp),
                    triggers: &config.triggers,
                },
            )?);
        }
        let events_endpoint = format!(
            "/repos/{}/issues/{}/events?per_page=100",
            location.repo, location.number
        );
        let events = self.json(&["api", &events_endpoint])?;
        let events = events.as_array().ok_or_else(|| {
            ProducerError::GitHub("GitHub issue events response must be an array".to_owned())
        })?;
        for event in events {
            let Some(event_trigger) = configured_gh_event(event, &config.triggers) else {
                continue;
            };
            let timestamp = event
                .get("created_at")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ProducerError::GitHub("GitHub trigger event omitted created_at".to_owned())
                })?;
            let event_id = event_trigger.id.clone();
            candidates.push(gh_api_candidate(
                "search",
                &item,
                self_actor,
                GhObservationHints {
                    repo: Some(&location.repo),
                    item_type: Some(location.item_type),
                    notification_reason: None,
                    event_id: Some(event_id),
                    triggering_comment: None,
                    event_trigger: Some(event_trigger),
                    trigger_timestamp: Some(timestamp),
                    triggers: &config.triggers,
                },
            )?);
        }
        candidates.sort_by_key(GhIntakeCandidate::dedup_identity);
        candidates.dedup_by(|right, left| right.dedup_identity() == left.dedup_identity());
        Ok(candidates)
    }

    pub(super) fn diagnostic_observation(
        &self,
        config: &GhProducer,
        item_url: &str,
        trigger_kind: &str,
        actor: &str,
        now: DateTime<Utc>,
    ) -> Result<GhObservation, ProducerError> {
        validate_login(actor, "GitHub diagnostic actor")?;
        let location = parse_gh_item_url(item_url).map_err(ProducerError::InvalidObservation)?;
        let viewer: Value = self.json(&["api", "user"])?;
        let self_actor = viewer
            .get("login")
            .and_then(Value::as_str)
            .filter(|login| !login.is_empty())
            .ok_or_else(|| {
                ProducerError::GitHub("gh api user omitted a non-empty login".to_owned())
            })?;
        let item_endpoint = match location.item_type {
            GhItemType::Issue => {
                format!("/repos/{}/issues/{}", location.repo, location.number)
            }
            GhItemType::PullRequest => {
                format!("/repos/{}/pulls/{}", location.repo, location.number)
            }
        };
        let item = self.json(&["api", &item_endpoint])?;
        let trigger_value = match trigger_kind {
            "command-comment" => config.triggers.command_comments.first(),
            "mention" => config.triggers.mentions.first(),
            "assignment" => config.triggers.assignments.first(),
            "label" => config.triggers.labels.first(),
            _ => {
                return Err(ProducerError::InvalidObservation(format!(
                    "unsupported GitHub diagnostic event {trigger_kind:?}"
                )))
            }
        }
        .ok_or_else(|| {
            ProducerError::InvalidConfig(format!(
                "GitHub diagnostic event {trigger_kind:?} has no configured trigger value"
            ))
        })?
        .clone();
        let timestamp = now.to_rfc3339();
        let diagnostic_id = format!(
            "diagnostic-{}",
            stable_key(&[
                "gh-diagnostic",
                item_url,
                trigger_kind,
                actor,
                &trigger_value,
                &timestamp,
            ])
        );
        let triggering_comment =
            matches!(trigger_kind, "command-comment" | "mention").then(|| GhTriggeringComment {
                id: diagnostic_id.clone(),
                author: actor.to_owned(),
                body: trigger_value.clone(),
            });
        let event_trigger =
            matches!(trigger_kind, "assignment" | "label").then(|| GhEventTrigger {
                id: diagnostic_id.clone(),
                kind: if trigger_kind == "assignment" {
                    "assignment"
                } else {
                    "label"
                },
                actor: actor.to_owned(),
                value: trigger_value,
            });
        let mut first_observation = None;
        for source in config.sources.iter().filter(|source| {
            matches!(source, GhSource::Search(_)) && source.constraints().has_identity_scope()
        }) {
            let candidate = gh_api_candidate(
                "search",
                &item,
                self_actor,
                GhObservationHints {
                    repo: Some(&location.repo),
                    item_type: Some(location.item_type),
                    notification_reason: None,
                    event_id: Some(diagnostic_id.clone()),
                    triggering_comment: triggering_comment.clone(),
                    event_trigger: event_trigger.clone(),
                    trigger_timestamp: Some(&timestamp),
                    triggers: &config.triggers,
                },
            )?;
            let GhIntakeCandidate::Observation(observation) = candidate else {
                return Err(ProducerError::InvalidObservation(
                    "configured GitHub diagnostic trigger could not be classified".to_owned(),
                ));
            };
            if gh_source_constraints_reason(source.constraints(), &observation).is_none() {
                return Ok(*observation);
            }
            first_observation.get_or_insert(*observation);
        }
        first_observation.ok_or_else(|| {
            ProducerError::InvalidConfig(
                "GitHub diagnostic requires at least one identity-scoped search source".to_owned(),
            )
        })
    }
}

pub(super) fn configured_gh_event(event: &Value, triggers: &GhTriggers) -> Option<GhEventTrigger> {
    let id = event
        .get("id")
        .and_then(json_identifier)
        .or_else(|| event.get("node_id").and_then(json_identifier))?;
    let actor = event
        .pointer("/actor/login")
        .and_then(Value::as_str)
        .filter(|actor| !actor.is_empty())?;
    let (kind, value) = match event.get("event").and_then(Value::as_str) {
        Some("assigned") => (
            "assignment",
            event.pointer("/assignee/login").and_then(Value::as_str)?,
        ),
        Some("labeled") => (
            "label",
            event.pointer("/label/name").and_then(Value::as_str)?,
        ),
        _ => return None,
    };
    let configured = match kind {
        "assignment" => triggers
            .assignments
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(value)),
        "label" => triggers
            .labels
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(value)),
        _ => false,
    };
    configured.then(|| GhEventTrigger {
        id,
        kind,
        actor: actor.to_owned(),
        value: value.to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GhIntakeCandidate {
    Observation(Box<GhObservation>),
    TriggerActorUnavailable { source: String, node_id: String },
}

impl GhIntakeCandidate {
    fn source(&self) -> &str {
        match self {
            Self::Observation(observation) => &observation.source,
            Self::TriggerActorUnavailable { source, .. } => source,
        }
    }

    fn dedup_identity(&self) -> String {
        match self {
            Self::Observation(observation) => format!(
                "{}:{}:{}",
                observation.trigger_kind,
                observation
                    .comment_id
                    .as_deref()
                    .or(observation.event_id.as_deref())
                    .unwrap_or_default(),
                observation.node_id
            ),
            Self::TriggerActorUnavailable { source, node_id } => {
                format!("unavailable:{source}:{node_id}")
            }
        }
    }

    const fn unavailable(&self) -> bool {
        matches!(self, Self::TriggerActorUnavailable { .. })
    }
}

pub(super) fn normalize_gh_candidates(
    config: &GhProducer,
    candidates: &mut Vec<GhIntakeCandidate>,
) {
    let source_filtered = |candidate: &GhIntakeCandidate| match candidate {
        GhIntakeCandidate::Observation(observation) => {
            gh_source_filter_reason(config, observation).is_some()
        }
        GhIntakeCandidate::TriggerActorUnavailable { .. } => true,
    };
    candidates.sort_by(|left, right| {
        left.dedup_identity()
            .cmp(&right.dedup_identity())
            .then_with(|| source_filtered(left).cmp(&source_filtered(right)))
            .then_with(|| left.unavailable().cmp(&right.unavailable()))
            .then_with(|| left.source().cmp(right.source()))
    });
    candidates.dedup_by(|right, left| right.dedup_identity() == left.dedup_identity());
}

pub(super) struct GhObservationHints<'a> {
    repo: Option<&'a str>,
    item_type: Option<GhItemType>,
    notification_reason: Option<String>,
    event_id: Option<String>,
    triggering_comment: Option<GhTriggeringComment>,
    event_trigger: Option<GhEventTrigger>,
    trigger_timestamp: Option<&'a str>,
    triggers: &'a GhTriggers,
}

#[derive(Clone)]
pub(super) struct GhEventTrigger {
    pub(super) id: String,
    pub(super) kind: &'static str,
    pub(super) actor: String,
    pub(super) value: String,
}

pub(super) fn gh_api_candidate(
    source: &str,
    item: &Value,
    self_actor: &str,
    hints: GhObservationHints<'_>,
) -> Result<GhIntakeCandidate, ProducerError> {
    let GhObservationHints {
        repo: repo_hint,
        item_type: item_type_hint,
        notification_reason,
        event_id,
        triggering_comment,
        event_trigger,
        trigger_timestamp,
        triggers,
    } = hints;
    let node_id = item
        .get("node_id")
        .and_then(Value::as_str)
        .filter(|node_id| !node_id.is_empty())
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted node_id".to_owned()))?;
    let item_author = item
        .pointer("/user/login")
        .and_then(Value::as_str)
        .filter(|actor| !actor.is_empty())
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted user.login".to_owned()))?;
    let repo = repo_hint
        .or_else(|| item.pointer("/base/repo/full_name").and_then(Value::as_str))
        .or_else(|| {
            item.get("repository_url")
                .and_then(Value::as_str)
                .and_then(repo_from_api_url)
        })
        .ok_or_else(|| {
            ProducerError::GitHub("GitHub issue/PR omitted repository identity".to_owned())
        })?;
    let number = item
        .get("number")
        .and_then(Value::as_u64)
        .filter(|number| *number > 0)
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted number".to_owned()))?;
    let html_url = item
        .get("html_url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted html_url".to_owned()))?;
    let item_type = item_type_hint.unwrap_or_else(|| {
        if item.get("pull_request").is_some() || item.get("head").is_some() {
            GhItemType::PullRequest
        } else {
            GhItemType::Issue
        }
    });
    let head_sha = (item_type == GhItemType::PullRequest)
        .then(|| {
            item.pointer("/head/sha")
                .and_then(Value::as_str)
                .filter(|sha| !sha.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| ProducerError::GitHub("GitHub PR omitted head.sha".to_owned()))
        })
        .transpose()?;
    let title = item
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| ProducerError::GitHub("GitHub issue/PR omitted title".to_owned()))?;
    let body = item.get("body").and_then(Value::as_str).unwrap_or_default();
    let labels = item
        .get("labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .map(|label| {
                    label
                        .get("name")
                        .and_then(Value::as_str)
                        .or_else(|| label.as_str())
                        .filter(|name| !name.is_empty())
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            ProducerError::GitHub("GitHub issue/PR label omitted a name".to_owned())
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let assignees = item
        .get("assignees")
        .and_then(Value::as_array)
        .map(|assignees| {
            assignees
                .iter()
                .map(|assignee| {
                    assignee
                        .get("login")
                        .and_then(Value::as_str)
                        .filter(|login| !login.is_empty())
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| {
                            ProducerError::GitHub(
                                "GitHub issue/PR assignee omitted login".to_owned(),
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let (comment_id, trigger_actor, trigger_kind, trigger_value, triggering_comment) =
        if let Some(triggering_comment) = triggering_comment {
            let trigger_kind = if triggers
                .command_comments
                .iter()
                .any(|command| command == triggering_comment.body.trim())
            {
                "command-comment"
            } else if triggers
                .mentions
                .iter()
                .any(|command| command == triggering_comment.body.trim())
            {
                "mention"
            } else {
                return Ok(GhIntakeCandidate::TriggerActorUnavailable {
                    source: source.to_owned(),
                    node_id: node_id.to_owned(),
                });
            };
            (
                Some(triggering_comment.id.clone()),
                triggering_comment.author.clone(),
                trigger_kind,
                None,
                Some(triggering_comment),
            )
        } else if let Some(event) = event_trigger {
            (None, event.actor, event.kind, Some(event.value), None)
        } else {
            return Ok(GhIntakeCandidate::TriggerActorUnavailable {
                source: source.to_owned(),
                node_id: node_id.to_owned(),
            });
        };
    let trigger_timestamp = trigger_timestamp.ok_or_else(|| {
        ProducerError::GitHub("GitHub trigger omitted an event timestamp".to_owned())
    })?;
    Ok(GhIntakeCandidate::Observation(Box::new(GhObservation {
        source: source.to_owned(),
        repo: repo.to_owned(),
        number,
        html_url: html_url.to_owned(),
        item_type,
        head_sha: head_sha.clone(),
        node_id: node_id.to_owned(),
        item_author: item_author.to_owned(),
        trigger_actor,
        self_actor: self_actor.to_owned(),
        notification_reason,
        trigger_kind: trigger_kind.to_owned(),
        event_id,
        comment_id,
        trigger_timestamp: trigger_timestamp.to_owned(),
        trigger_value,
        context: GhContextSnapshot {
            schema_version: GH_CONTEXT_SCHEMA_VERSION,
            title: title.to_owned(),
            body: body.to_owned(),
            state: Some(gh_item_state(item)?),
            head_sha: head_sha.clone(),
            labels,
            assignees,
            triggering_comment,
        },
    })))
}

pub(super) fn exact_notification_comment(
    notification: &Value,
    latest_comment_url: &str,
    comment: &Value,
) -> Option<GhTriggeringComment> {
    let triggering_comment = gh_triggering_comment(comment)?;
    if latest_comment_url.rsplit('/').next()? != triggering_comment.id {
        return None;
    }
    let notification_updated_at = notification.get("updated_at")?.as_str()?;
    comment
        .get("updated_at")
        .and_then(Value::as_str)
        .is_some_and(|comment_at| gh_timestamps_equal(notification_updated_at, comment_at))
        .then_some(triggering_comment)
}

pub(super) fn gh_comment_timestamp(comment: &Value) -> Option<&str> {
    comment
        .get("updated_at")
        .and_then(Value::as_str)
        .or_else(|| comment.get("created_at").and_then(Value::as_str))
}

pub(super) fn gh_timestamps_equal(left: &str, right: &str) -> bool {
    DateTime::parse_from_rfc3339(left)
        .ok()
        .zip(DateTime::parse_from_rfc3339(right).ok())
        .is_some_and(|(left, right)| left == right)
}

pub(super) fn gh_triggering_comment(comment: &Value) -> Option<GhTriggeringComment> {
    let id = comment
        .get("id")
        .and_then(json_identifier)
        .filter(|id| !id.is_empty())?;
    let author = comment
        .pointer("/user/login")
        .and_then(Value::as_str)
        .filter(|author| !author.is_empty())?;
    let body = comment.get("body").and_then(Value::as_str)?;
    Some(GhTriggeringComment {
        id,
        author: author.to_owned(),
        body: body.to_owned(),
    })
}

pub(super) fn comment_is_configured_trigger(triggers: &GhTriggers, body: &str) -> bool {
    let body = body.trim();
    triggers
        .command_comments
        .iter()
        .chain(triggers.mentions.iter())
        .any(|command| command == body)
}

pub(super) fn json_identifier(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

pub(super) fn repo_from_api_url(url: &str) -> Option<&str> {
    url.split_once("/repos/")
        .map(|(_, repo)| repo)
        .filter(|repo| {
            let mut parts = repo.split('/');
            parts.next().is_some_and(|part| !part.is_empty())
                && parts.next().is_some_and(|part| !part.is_empty())
                && parts.next().is_none()
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GhItemLocation {
    repo: String,
    number: u64,
    item_type: GhItemType,
}

pub(super) fn parse_gh_item_url(url: &str) -> Result<GhItemLocation, String> {
    let location = url
        .strip_prefix("https://")
        .ok_or_else(|| "URL must use HTTPS".to_owned())?;
    let (host, path) = location
        .split_once('/')
        .ok_or_else(|| "URL must contain a host and item path".to_owned())?;
    if host != "github.com" {
        return Err("URL host must be github.com".to_owned());
    }
    if path.contains(['?', '#']) {
        return Err("URL must not contain a query or fragment".to_owned());
    }
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() != 4 || parts.iter().any(|part| part.is_empty()) {
        return Err("URL path must be owner/repo/issues|pull/number".to_owned());
    }
    let repo = format!("{}/{}", parts[0], parts[1]);
    validate_repo_constraint(&repo).map_err(|error| error.to_string())?;
    let item_type = match parts[2] {
        "issues" => GhItemType::Issue,
        "pull" => GhItemType::PullRequest,
        _ => return Err("URL must identify an issue or pull request".to_owned()),
    };
    let number = parts[3]
        .parse::<u64>()
        .ok()
        .filter(|number| *number > 0)
        .ok_or_else(|| "URL item number must be positive".to_owned())?;
    Ok(GhItemLocation {
        repo,
        number,
        item_type,
    })
}

pub(super) fn gh_item_state(item: &Value) -> Result<GhItemState, ProducerError> {
    match item.get("state").and_then(Value::as_str) {
        Some(state) if state.eq_ignore_ascii_case("open") => Ok(GhItemState::Open),
        Some(state) if state.eq_ignore_ascii_case("closed") => Ok(GhItemState::Closed),
        _ => Err(ProducerError::GitHub(
            "GitHub issue/PR omitted a supported state".to_owned(),
        )),
    }
}

pub(super) fn gh_search_queries(constraints: &GhSourceConstraints) -> Vec<String> {
    let mut scopes = BTreeSet::new();
    if let Some(repo) = &constraints.repo {
        scopes.insert(format!("repo:{repo}"));
    }
    scopes.extend(
        constraints
            .repositories
            .iter()
            .map(|repo| format!("repo:{repo}")),
    );
    for owner in &constraints.owners {
        scopes.insert(format!("org:{owner}"));
        scopes.insert(format!("user:{owner}"));
    }
    let mut filters = Vec::new();
    filters.extend(
        constraints
            .labels
            .iter()
            .map(|label| format!("label:{}", quote_gh_query_value(label))),
    );
    if let Some(state) = constraints.state {
        filters.push(format!(
            "state:{}",
            match state {
                GhItemState::Open => "open",
                GhItemState::Closed => "closed",
            }
        ));
    }
    if let Some(assignee) = &constraints.assignee {
        filters.push(format!("assignee:{}", quote_gh_query_value(assignee)));
    }
    if constraints.kinds.len() == 1 {
        filters.push(format!(
            "is:{}",
            match constraints.kinds[0] {
                GhSourceItemKind::Issue => "issue",
                GhSourceItemKind::PullRequest => "pr",
            }
        ));
    }
    if let Some(query) = &constraints.query {
        filters.push(query.clone());
    }
    scopes
        .into_iter()
        .map(|scope| {
            std::iter::once(scope)
                .chain(filters.iter().cloned())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

pub(super) fn quote_gh_query_value(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('\"', "\\\""))
}

impl GhMutationSink for GhCliMutationSink {
    fn post_evidence(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
        if mutation.state != "COMPLETED" {
            return Err(format!(
                "refusing GitHub mutation state {:?}; expected COMPLETED",
                mutation.state
            ));
        }
        let completion_id = mutation
            .completion_id
            .as_deref()
            .ok_or_else(|| "concrete GitHub mutation requires a durable completionId".to_owned())?;
        let remote_key = stable_key(&["gh-remote-completion", completion_id]);
        let remote_marker = format!("<!-- tally-completion:{remote_key} -->");
        let encoded = serde_json::to_string(mutation)
            .map_err(|error| format!("cannot encode GitHub evidence: {error}"))?;
        let body = mutation.assisted_by.as_ref().map_or_else(
            || format!("{remote_marker}\n{encoded}"),
            |assisted_by| format!("{remote_marker}\n{encoded}\n\n{}", assisted_by.trailer()),
        );
        let (kind, state, comment_exists) =
            self.completion_state(&mutation.item_id, &remote_marker)?;
        if !comment_exists {
            self.graphql(serde_json::json!({
                "query": GH_COMPLETION_COMMENT_GRAPHQL,
                "variables": {"itemId": mutation.item_id, "body": body},
            }))?;
        }
        if !matches!(state.as_str(), "OPEN" | "CLOSED" | "MERGED") {
            return Err(format!(
                "GitHub {kind} {:?} has unsupported state {state:?}",
                mutation.item_id
            ));
        }
        Ok(())
    }

    fn close_item(&mut self, mutation: &GhCompletedMutation) -> Result<(), String> {
        if mutation.state != "COMPLETED" {
            return Err(format!(
                "refusing GitHub mutation state {:?}; expected COMPLETED",
                mutation.state
            ));
        }
        let completion_id = mutation
            .completion_id
            .as_deref()
            .ok_or_else(|| "concrete GitHub mutation requires a durable completionId".to_owned())?;
        let remote_key = stable_key(&["gh-remote-completion", completion_id]);
        let remote_marker = format!("<!-- tally-completion:{remote_key} -->");
        let (kind, state, _) = self.completion_state(&mutation.item_id, &remote_marker)?;
        if state == "OPEN" {
            let query = if kind == "Issue" {
                GH_COMPLETION_ISSUE_GRAPHQL
            } else {
                GH_COMPLETION_PULL_REQUEST_GRAPHQL
            };
            self.graphql(serde_json::json!({
                "query": query,
                "variables": {"itemId": mutation.item_id},
            }))?;
        } else if !matches!(state.as_str(), "CLOSED" | "MERGED") {
            return Err(format!(
                "GitHub {kind} {:?} has unsupported state {state:?}",
                mutation.item_id
            ));
        }
        Ok(())
    }
}

impl GhAcknowledgementSink for GhCliAcknowledgementSink {
    fn post_acknowledgement(
        &mut self,
        acknowledgement: &GhTriggerAcknowledgement,
    ) -> Result<(), String> {
        let decision = match acknowledgement.decision {
            GhDecisionStatus::Accepted => "accepted",
            GhDecisionStatus::Filtered => "filtered",
            GhDecisionStatus::Duplicate => "duplicate",
            _ => {
                return Err(format!(
                    "refusing to acknowledge non-terminal trigger intake decision {:?}",
                    acknowledgement.decision
                ))
            }
        };
        let marker = format!(
            "<!-- tally-trigger:{}:{decision} -->",
            acknowledgement.receipt_id
        );
        let summary = match acknowledgement.decision {
            GhDecisionStatus::Accepted => "Tally accepted this trigger.",
            GhDecisionStatus::Filtered => "Tally filtered this trigger by policy.",
            GhDecisionStatus::Duplicate => "Tally already recorded this trigger.",
            _ => unreachable!("decision was narrowed above"),
        };
        let mut body = format!("{marker}\n{summary}");
        if let Some(task_uuid) = &acknowledgement.task_uuid {
            body.push_str(&format!("\n\nTask: `{task_uuid}`"));
        }
        if let Some(pointer) = &acknowledgement.status_pointer {
            body.push_str(&format!("\nStatus: `{pointer}`"));
        }
        let (_, state, exists) = self
            .mutation
            .completion_state(&acknowledgement.item_id, &marker)?;
        if !exists {
            self.mutation.graphql(serde_json::json!({
                "query": GH_COMPLETION_COMMENT_GRAPHQL,
                "variables": {"itemId": acknowledgement.item_id, "body": body},
            }))?;
        }
        if !matches!(state.as_str(), "OPEN" | "CLOSED" | "MERGED") {
            return Err(format!(
                "GitHub item {:?} has unsupported state {state:?}",
                acknowledgement.item_id
            ));
        }
        Ok(())
    }
}

impl GhCliMutationSink {
    fn completion_state(
        &self,
        item_id: &str,
        remote_marker: &str,
    ) -> Result<(String, String, bool), String> {
        let mut cursor = None::<String>;
        let mut identity = None::<(String, String)>;
        for _ in 0..MAX_GH_COMMENT_PAGES {
            let response = self.graphql(serde_json::json!({
                "query": GH_COMPLETION_STATE_GRAPHQL,
                "variables": {"itemId": item_id, "cursor": cursor},
            }))?;
            let node = response
                .pointer("/data/node")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    format!("GitHub item {item_id:?} did not resolve to an Issue or PullRequest")
                })?;
            let kind = node
                .get("__typename")
                .and_then(Value::as_str)
                .ok_or_else(|| "GitHub completion query omitted node __typename".to_owned())?;
            if !matches!(kind, "Issue" | "PullRequest") {
                return Err(format!(
                    "GitHub item {item_id:?} has unsupported node kind {kind:?}"
                ));
            }
            let state = node
                .get("state")
                .and_then(Value::as_str)
                .ok_or_else(|| "GitHub completion query omitted node state".to_owned())?;
            let current = (kind.to_owned(), state.to_owned());
            if identity
                .as_ref()
                .is_some_and(|identity| identity != &current)
            {
                return Err("GitHub completion identity changed during pagination".to_owned());
            }
            identity = Some(current);
            let comments = node
                .get("comments")
                .ok_or_else(|| "GitHub completion query omitted comments connection".to_owned())?;
            if comments
                .get("nodes")
                .and_then(Value::as_array)
                .is_some_and(|comments| {
                    comments.iter().any(|comment| {
                        comment
                            .get("body")
                            .and_then(Value::as_str)
                            .is_some_and(|comment| comment.contains(remote_marker))
                    })
                })
            {
                let (kind, state) = identity.expect("identity was assigned above");
                return Ok((kind, state, true));
            }
            let page_info = comments
                .get("pageInfo")
                .and_then(Value::as_object)
                .ok_or_else(|| "GitHub completion query omitted comments pageInfo".to_owned())?;
            if !page_info
                .get("hasNextPage")
                .and_then(Value::as_bool)
                .ok_or_else(|| "GitHub comments pageInfo omitted hasNextPage".to_owned())?
            {
                let (kind, state) = identity.expect("identity was assigned above");
                return Ok((kind, state, false));
            }
            cursor = Some(
                page_info
                    .get("endCursor")
                    .and_then(Value::as_str)
                    .filter(|cursor| !cursor.is_empty())
                    .ok_or_else(|| {
                        "GitHub comments pageInfo omitted a continuation cursor".to_owned()
                    })?
                    .to_owned(),
            );
        }
        Err(format!(
            "GitHub item {item_id:?} exceeds the {MAX_GH_COMMENT_PAGES}-page completion scan cap; refusing a possibly duplicate comment"
        ))
    }

    fn graphql(&self, request: Value) -> Result<Value, String> {
        let request = serde_json::to_vec(&request)
            .map_err(|error| format!("cannot encode GitHub GraphQL request: {error}"))?;
        let output = run_gh_bounded(
            &self.program,
            &["api", "graphql", "--input", "-"],
            Some(request),
        )?;
        let response: Value = serde_json::from_slice(&output)
            .map_err(|error| format!("gh api graphql returned invalid JSON: {error}"))?;
        if response
            .get("errors")
            .and_then(Value::as_array)
            .is_some_and(|errors| !errors.is_empty())
        {
            return Err(format!(
                "gh api graphql returned errors: {}",
                response["errors"]
            ));
        }
        Ok(response)
    }
}

pub(super) fn run_gh_bounded(
    program: &Path,
    args: &[&str],
    input: Option<Vec<u8>>,
) -> Result<Vec<u8>, String> {
    run_gh_bounded_with_timeout(program, args, input, GH_PROCESS_TIMEOUT)
}

pub(super) fn run_gh_bounded_with_timeout(
    program: &Path,
    args: &[&str],
    input: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    #[cfg(target_os = "linux")]
    // SAFETY: this pre-exec hook performs only async-signal-safe libc calls.
    unsafe {
        command.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() == 1 {
                return Err(std::io::Error::from_raw_os_error(libc::ECHILD));
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("cannot execute {}: {error}", program.display()))?;
    let process_group = match i32::try_from(child.id()) {
        Ok(process_group) => process_group,
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{} returned an unrepresentable process-group id",
                program.display()
            ));
        }
    };
    let stdin_task = input.map(|input| {
        let mut stdin = child.stdin.take().expect("requested piped gh stdin");
        thread::spawn(move || -> std::io::Result<()> {
            stdin.write_all(&input)?;
            drop(stdin);
            Ok(())
        })
    });
    let stdout_task = bounded_reader(
        child.stdout.take().expect("requested piped gh stdout"),
        MAX_GH_PROCESS_OUTPUT_BYTES,
    );
    let stderr_task = bounded_reader(
        child.stderr.take().expect("requested piped gh stderr"),
        MAX_GH_PROCESS_OUTPUT_BYTES,
    );
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut group_kill_error = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => {
                let _ = kill_gh_process_group(process_group);
                let _ = child.kill();
                break match child.wait() {
                    Ok(_) => Err(format!("cannot poll {}: {error}", program.display())),
                    Err(wait_error) => Err(format!(
                        "cannot poll {} ({error}) or reap it after cleanup: {wait_error}",
                        program.display()
                    )),
                };
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            group_kill_error = kill_gh_process_group(process_group).err();
            let _ = child.kill();
            break child
                .wait()
                .map_err(|error| format!("cannot reap timed-out {}: {error}", program.display()));
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdin_result = if let Some(task) = stdin_task {
        match task.join() {
            Ok(result) => result.map_err(|error| format!("cannot write gh stdin: {error}")),
            Err(_) => Err("gh stdin writer panicked".to_owned()),
        }
    } else {
        Ok(())
    };
    let stdout = stdout_task.drain("stdout");
    let stderr = stderr_task.drain("stderr");
    if timed_out {
        let cleanup = group_kill_error
            .map(|error| format!("; process-group cleanup failed: {error}"))
            .unwrap_or_default();
        return Err(format!(
            "{} exceeded the {} second timeout{cleanup}",
            program.display(),
            timeout.as_secs_f64()
        ));
    }
    stdin_result?;
    let status = status?;
    let (stdout, stdout_overflow) = stdout?;
    let (stderr, stderr_overflow) = stderr?;
    if stdout_overflow || stderr_overflow {
        return Err(format!(
            "{} output exceeded the {} byte cap",
            program.display(),
            MAX_GH_PROCESS_OUTPUT_BYTES
        ));
    }
    if !status.success() {
        return Err(format!(
            "{} exited {status}: {}",
            program.display(),
            String::from_utf8_lossy(&stderr).trim()
        ));
    }
    Ok(stdout)
}

pub(super) fn kill_gh_process_group(process_group: i32) -> Result<(), String> {
    // SAFETY: process_group is the positive, representable pid returned by the
    // child, and negating it targets only that child's process group.
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(format!(
            "cannot kill gh process group {process_group}: {error}"
        ))
    }
}

pub(super) struct BoundedReaderTask {
    result: mpsc::Receiver<std::io::Result<(Vec<u8>, bool)>>,
    thread: thread::JoinHandle<()>,
}

impl BoundedReaderTask {
    fn drain(self, stream: &str) -> Result<(Vec<u8>, bool), String> {
        let Self { result, thread } = self;
        match result.recv_timeout(GH_READER_DRAIN_TIMEOUT) {
            Ok(result) => {
                thread
                    .join()
                    .map_err(|_| format!("gh {stream} reader panicked"))?;
                result.map_err(|error| format!("cannot read gh {stream}: {error}"))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                drop(thread);
                Err(format!(
                    "gh {stream} reader exceeded the {} second drain timeout",
                    GH_READER_DRAIN_TIMEOUT.as_secs()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                thread
                    .join()
                    .map_err(|_| format!("gh {stream} reader panicked"))?;
                Err(format!("gh {stream} reader ended without a result"))
            }
        }
    }
}

pub(super) fn bounded_reader(
    mut reader: impl Read + Send + 'static,
    limit: usize,
) -> BoundedReaderTask {
    let (sender, result) = mpsc::sync_channel(1);
    let thread = thread::spawn(move || {
        let mut kept = Vec::new();
        let mut overflow = false;
        let mut buffer = [0_u8; 8192];
        let read_result = loop {
            match reader.read(&mut buffer) {
                Ok(0) => break Ok((kept, overflow)),
                Ok(read) => {
                    let remaining = limit.saturating_sub(kept.len());
                    kept.extend_from_slice(&buffer[..read.min(remaining)]);
                    overflow |= read > remaining;
                }
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(read_result);
    });
    BoundedReaderTask { result, thread }
}
