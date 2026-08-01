use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReachabilityStable {
    Reachable,
    Lost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReachabilityTransition {
    Lost,
    Returned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReachabilityOutcome {
    pub stable: ReachabilityStable,
    pub transition: Option<ReachabilityTransition>,
    pub generation: u64,
    pub emitted: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct ReachabilityState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) probe_pool: Option<String>,
    pub(super) stable: ReachabilityStable,
    pub(super) candidate_reachable: Option<bool>,
    pub(super) consecutive: u32,
    pub(super) generation: u64,
    #[serde(default)]
    pub(super) notified_generation: u64,
}

impl Default for ReachabilityState {
    fn default() -> Self {
        Self {
            probe_pool: None,
            stable: ReachabilityStable::Reachable,
            candidate_reachable: None,
            consecutive: 0,
            generation: 0,
            notified_generation: 0,
        }
    }
}

pub struct ProducerEngine<'a> {
    registry: &'a BTreeMap<String, ProducerConfig>,
    events_dir: PathBuf,
    state_dir: PathBuf,
    brief_root: PathBuf,
}

impl<'a> ProducerEngine<'a> {
    pub fn new(
        registry: &'a BTreeMap<String, ProducerConfig>,
        events_dir: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
        brief_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registry,
            events_dir: events_dir.into(),
            state_dir: state_dir.into(),
            brief_root: brief_root.into(),
        }
    }

    pub fn producer_kind(&self, producer: &str) -> Result<&'static str, ProducerError> {
        Ok(self.get(producer)?.kind())
    }

    pub fn emit_calendar(
        &self,
        producer: &str,
        now: DateTime<Utc>,
    ) -> Result<EmitOutcome, ProducerError> {
        let ProducerConfig::Calendar(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "calendar"));
        };
        let payload = config
            .enqueue
            .payload(EnqueueSource::Calendar, Some(producer), now, None)?;
        let name = format!("{producer}-calendar-{}{}", Uuid::new_v4(), INGRESS_SUFFIX);
        self.emit_named(&name, &payload)
    }

    pub fn emit_gh(
        &self,
        producer: &str,
        observation: &GhObservation,
        now: DateTime<Utc>,
    ) -> Result<EmitOutcome, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(EmitOutcome::Disabled);
        }
        validate_gh_observation(producer, config, observation)?;
        if let Some(reason) = gh_filter_reason(config, observation) {
            return Ok(EmitOutcome::Filtered { reason });
        }
        let origin = gh_origin(producer, config, observation);
        let mut payload =
            config
                .enqueue
                .payload(EnqueueSource::Gh, Some(producer), now, Some(&origin))?;
        payload.dedup_key = Some(gh_trigger_dedup_key(&origin)?);
        payload.gh_trigger_actor = Some(observation.trigger_actor.clone());
        payload.gh_self_actor = Some(observation.self_actor.clone());
        payload.task_uuid = Some(gh_trigger_task_uuid(&origin)?.to_string());
        let key = gh_trigger_receipt_id(&origin)?;
        payload.gh_origin = Some(origin);
        self.emit_named(&format!("{producer}-gh-{key}{INGRESS_SUFFIX}"), &payload)
    }

    pub fn poll_gh(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        now: DateTime<Utc>,
    ) -> Result<Vec<EmitOutcome>, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(Vec::new());
        }
        intake
            .poll(config)?
            .iter()
            .map(|candidate| match candidate {
                GhIntakeCandidate::Observation(observation) => {
                    self.emit_gh(producer, observation, now)
                }
                GhIntakeCandidate::TriggerActorUnavailable { .. } => Ok(EmitOutcome::Filtered {
                    reason: GhFilterReason::TriggerActorUnavailable,
                }),
            })
            .collect()
    }

    pub fn preview_gh(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        now: DateTime<Utc>,
    ) -> Result<Vec<GhDecision>, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(Vec::new());
        }
        let mut decisions = Vec::new();
        for candidate in intake.poll(config)? {
            match candidate {
                GhIntakeCandidate::Observation(observation) => {
                    decisions.push(
                        match self.preview_gh_observation(producer, &observation, now) {
                            Ok(decision) => decision,
                            Err(error) => malformed_gh_decision(
                                producer,
                                GhCandidateSummary::from_observation(&observation),
                                error.to_string(),
                            ),
                        },
                    );
                }
                GhIntakeCandidate::TriggerActorUnavailable { source, node_id } => {
                    decisions.push(unavailable_actor_decision(producer, source, node_id));
                }
            }
        }
        Ok(decisions)
    }

    pub fn explain_gh(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        item_url: &str,
        now: DateTime<Utc>,
    ) -> Result<Vec<GhDecision>, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        let mut decisions = Vec::new();
        for candidate in intake.item(config, item_url)? {
            match candidate {
                GhIntakeCandidate::Observation(observation) => {
                    decisions.push(
                        match self.preview_gh_observation(producer, &observation, now) {
                            Ok(decision) => decision,
                            Err(error) => malformed_gh_decision(
                                producer,
                                GhCandidateSummary::from_observation(&observation),
                                error.to_string(),
                            ),
                        },
                    );
                }
                GhIntakeCandidate::TriggerActorUnavailable { source, node_id } => {
                    decisions.push(unavailable_actor_decision(producer, source, node_id));
                }
            }
        }
        Ok(decisions)
    }

    pub fn diagnostic_gh_observation(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        item_url: &str,
        trigger_kind: &str,
        actor: &str,
        now: DateTime<Utc>,
    ) -> Result<GhObservation, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        intake.diagnostic_observation(config, item_url, trigger_kind, actor, now)
    }

    pub fn poll_gh_with_acknowledgements(
        &self,
        producer: &str,
        intake: &GhCliIntake,
        now: DateTime<Utc>,
        sink: &mut dyn GhAcknowledgementSink,
    ) -> Result<Vec<GhDecision>, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(Vec::new());
        }
        let mut decisions = Vec::new();
        for candidate in intake.poll(config)? {
            match candidate {
                GhIntakeCandidate::Observation(observation) => {
                    decisions.push(
                        match self.admit_gh_observation(producer, &observation, now, sink) {
                            Ok(decision) => decision,
                            Err(ProducerError::InvalidObservation(detail)) => {
                                malformed_gh_decision(
                                    producer,
                                    GhCandidateSummary::from_observation(&observation),
                                    detail,
                                )
                            }
                            Err(error) => return Err(error),
                        },
                    );
                }
                GhIntakeCandidate::TriggerActorUnavailable { source, node_id } => {
                    decisions.push(unavailable_actor_decision(producer, source, node_id));
                }
            }
        }
        Ok(decisions)
    }

    pub fn preview_gh_observation(
        &self,
        producer: &str,
        observation: &GhObservation,
        now: DateTime<Utc>,
    ) -> Result<GhDecision, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(disabled_gh_decision(producer, observation));
        }
        validate_gh_observation(producer, config, observation)?;
        let origin = gh_origin(producer, config, observation);
        let receipt_id = gh_trigger_receipt_id(&origin)?;
        let task_uuid = gh_trigger_task_uuid(&origin)?.to_string();
        if let Some(receipt) = self.read_gh_receipt(&receipt_id)? {
            return Ok(duplicate_gh_decision(
                producer,
                observation,
                &receipt_id,
                receipt.task_uuid,
            ));
        }
        if let Some(rule) = gh_filter_reason(config, observation) {
            return Ok(filtered_gh_decision(
                producer,
                observation,
                receipt_id,
                rule,
            ));
        }
        would_enqueue_gh_decision(
            producer,
            config,
            observation,
            origin,
            receipt_id,
            task_uuid,
            now,
        )
    }

    pub fn admit_gh_observation(
        &self,
        producer: &str,
        observation: &GhObservation,
        now: DateTime<Utc>,
        sink: &mut dyn GhAcknowledgementSink,
    ) -> Result<GhDecision, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "gh"));
        };
        if !config.enable {
            return Ok(disabled_gh_decision(producer, observation));
        }
        validate_gh_observation(producer, config, observation)?;
        let origin = gh_origin(producer, config, observation);
        let receipt_id = gh_trigger_receipt_id(&origin)?;
        let task_uuid = gh_trigger_task_uuid(&origin)?.to_string();
        let receipts_dir = self.state_dir.join("producers/gh-triggers");
        create_dir_durable(&receipts_dir)?;
        let lock_path = receipts_dir.join(format!("{receipt_id}.lock"));
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let receipt_path = receipts_dir.join(format!("{receipt_id}.json"));
        let (mut receipt, decision, needs_acknowledgement) = if path_lexists(&receipt_path)? {
            let mut receipt: GhTriggerReceipt =
                serde_json::from_slice(&read_bounded_regular(&receipt_path, 64 * 1024)?)?;
            validate_receipt_identity(&receipt, producer, observation, &receipt_id)?;
            if receipt.primary_acknowledged {
                receipt.duplicate_count = receipt.duplicate_count.saturating_add(1);
                let needs_acknowledgement = !receipt.duplicate_acknowledged;
                let decision = duplicate_gh_decision(
                    producer,
                    observation,
                    &receipt_id,
                    receipt.task_uuid.clone(),
                );
                (receipt, decision, needs_acknowledgement)
            } else {
                let decision =
                    primary_receipt_decision(producer, config, observation, &receipt, now)?;
                (receipt, decision, true)
            }
        } else {
            let rule = gh_filter_reason(config, observation);
            let (primary_decision, ingress, receipt_task) = if rule.is_some() {
                (GhDecisionStatus::Filtered, None, None)
            } else {
                let ingress = match self.emit_gh(producer, observation, now)? {
                    EmitOutcome::Emitted(path) => Some(path),
                    EmitOutcome::Duplicate => None,
                    EmitOutcome::Filtered { reason } => {
                        return Err(ProducerError::InvalidObservation(format!(
                            "GitHub trigger changed filter decision while locked: {reason:?}"
                        )))
                    }
                    EmitOutcome::Disabled => {
                        return Ok(disabled_gh_decision(producer, observation))
                    }
                };
                (GhDecisionStatus::Accepted, ingress, Some(task_uuid.clone()))
            };
            let receipt = GhTriggerReceipt {
                schema_version: 1,
                receipt_id: receipt_id.clone(),
                producer: producer.to_owned(),
                source: observation.source.clone(),
                item_id: observation.node_id.clone(),
                event_id: observation
                    .event_id
                    .clone()
                    .expect("current observations require eventId"),
                comment_id: observation.comment_id.clone(),
                trigger_kind: observation.trigger_kind.clone(),
                trigger_actor: observation.trigger_actor.clone(),
                trigger_timestamp: observation.trigger_timestamp.clone(),
                trigger_value: observation.trigger_value.clone(),
                primary_decision,
                rule,
                task_uuid: receipt_task,
                primary_acknowledged: false,
                duplicate_acknowledged: false,
                duplicate_count: 0,
            };
            let decision = match primary_decision {
                GhDecisionStatus::Accepted => accepted_gh_decision(
                    producer,
                    config,
                    observation,
                    origin,
                    receipt_id.clone(),
                    task_uuid,
                    ingress,
                    now,
                )?,
                GhDecisionStatus::Filtered => filtered_gh_decision(
                    producer,
                    observation,
                    receipt_id.clone(),
                    rule.expect("filtered receipt carries its rule"),
                ),
                _ => unreachable!("receipt primary decisions are accepted or filtered"),
            };
            (receipt, decision, true)
        };
        write_json_atomic(&receipt_path, &receipt)?;
        if needs_acknowledgement {
            if config.post_receipt && !config.never_mutate {
                let acknowledgement = acknowledgement_for_decision(&decision, observation)?;
                sink.post_acknowledgement(&acknowledgement)
                    .map_err(ProducerError::Acknowledgement)?;
            }
            match decision.decision {
                GhDecisionStatus::Duplicate => receipt.duplicate_acknowledged = true,
                GhDecisionStatus::Accepted | GhDecisionStatus::Filtered => {
                    receipt.primary_acknowledged = true
                }
                _ => {}
            }
            write_json_atomic(&receipt_path, &receipt)?;
        }
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })?;
        Ok(decision)
    }

    fn read_gh_receipt(&self, receipt_id: &str) -> Result<Option<GhTriggerReceipt>, ProducerError> {
        let path = self
            .state_dir
            .join("producers/gh-triggers")
            .join(format!("{receipt_id}.json"));
        if !path_lexists(&path)? {
            return Ok(None);
        }
        Ok(Some(serde_json::from_slice(&read_bounded_regular(
            &path,
            64 * 1024,
        )?)?))
    }

    pub fn validate_gh_origin(&self, origin: &GhOrigin) -> Result<(), ProducerError> {
        origin
            .validate()
            .map_err(|error| ProducerError::InvalidObservation(error.to_string()))?;
        let ProducerConfig::Gh(config) = self.get(&origin.producer)? else {
            return Err(self.kind_mismatch(&origin.producer, "gh"));
        };
        if !config.enable {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} is disabled",
                origin.producer
            )));
        }
        if origin.actor_exclude != config.actor_exclude {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin actorExclude does not match configuration",
                origin.producer
            )));
        }
        if !config
            .sources
            .iter()
            .any(|source| source.kind() == origin.source)
        {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin source {:?} is not configured",
                origin.producer, origin.source
            )));
        }
        if origin.schema_version == 0 {
            let excluded = if origin.actor_exclude == "self" {
                origin.trigger_actor == origin.self_actor
            } else {
                origin.trigger_actor == origin.actor_exclude
            };
            if excluded {
                return Err(ProducerError::InvalidObservation(format!(
                    "gh producer {:?} legacy origin actor is excluded",
                    origin.producer
                )));
            }
            return Ok(());
        }
        if origin.schema_version == 1 {
            if origin.allow_self_triggered != config.allow_self_triggered
                || origin.allowed_actors.iter().collect::<BTreeSet<_>>()
                    != config.allowed_actors.iter().collect::<BTreeSet<_>>()
            {
                return Err(ProducerError::InvalidObservation(format!(
                    "gh producer {:?} origin actor policy does not match configuration",
                    origin.producer
                )));
            }
            let excluded = (!origin.allowed_actors.is_empty()
                && !origin
                    .allowed_actors
                    .iter()
                    .any(|actor| actor.eq_ignore_ascii_case(&origin.trigger_actor)))
                || (origin.trigger_actor == origin.self_actor && !origin.allow_self_triggered)
                || (origin.actor_exclude != "self"
                    && origin
                        .trigger_actor
                        .eq_ignore_ascii_case(&origin.actor_exclude));
            if excluded {
                return Err(ProducerError::InvalidObservation(format!(
                    "gh producer {:?} legacy origin trigger actor is filtered",
                    origin.producer
                )));
            }
            return Ok(());
        }
        if origin.allow_self_triggered != config.allow_self_triggered
            || origin.allowed_actors.iter().collect::<BTreeSet<_>>()
                != config.allowed_actors.iter().collect::<BTreeSet<_>>()
        {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin actor policy does not match configuration",
                origin.producer
            )));
        }
        let observation = gh_observation(origin)?;
        validate_gh_observation(&origin.producer, config, &observation)?;
        if let Some(reason) = gh_filter_reason(config, &observation) {
            return Err(ProducerError::InvalidObservation(format!(
                "gh producer {:?} origin trigger actor is filtered: {reason:?}",
                origin.producer,
            )));
        }
        Ok(())
    }

    pub fn complete_gh(
        &self,
        origin: &GhOrigin,
        verdict: Verdict,
        evidence: Option<Value>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        self.complete_gh_with_id(origin, None, verdict, evidence, None, sink)
    }

    pub fn complete_gh_with_completion(
        &self,
        origin: &GhOrigin,
        verdict: Verdict,
        evidence: Option<Value>,
        completion: Option<SemanticCompletion>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        self.complete_gh_with_id(origin, None, verdict, evidence, completion, sink)
    }

    pub fn complete_gh_once(
        &self,
        origin: &GhOrigin,
        completion_id: &str,
        verdict: Verdict,
        evidence: Option<Value>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        self.complete_gh_once_with_completion(origin, completion_id, verdict, evidence, None, sink)
    }

    pub fn complete_gh_once_with_completion(
        &self,
        origin: &GhOrigin,
        completion_id: &str,
        verdict: Verdict,
        evidence: Option<Value>,
        completion: Option<SemanticCompletion>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        if completion_id.trim().is_empty()
            || completion_id.len() > MAX_GH_ORIGIN_FIELD_BYTES
            || completion_id.chars().any(char::is_control)
        {
            return Err(ProducerError::InvalidObservation(
                format!(
                    "GitHub completion id must be non-empty, at most {MAX_GH_ORIGIN_FIELD_BYTES} bytes, and contain no control characters"
                ),
            ));
        }
        let completed_dir = self.state_dir.join("producers/gh-completed");
        create_dir_durable(&completed_dir)?;
        let lock_path = completed_dir.join("mutations.lock");
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let marker_key = stable_key(&[
            "gh-completed",
            &origin.producer,
            &origin.source,
            &origin.node_id,
            completion_id,
        ]);
        let marker_path = completed_dir.join(format!("{marker_key}.json"));
        if path_lexists(&marker_path)? {
            let marker: GhCompletionMarker =
                serde_json::from_slice(&read_bounded_regular(&marker_path, 64 * 1024)?)?;
            if marker.completion_id != completion_id
                || marker.producer != origin.producer
                || marker.source != origin.source
                || marker.item_id != origin.node_id
            {
                return Err(ProducerError::InvalidObservation(format!(
                    "GitHub completion marker {} does not match its identity",
                    marker_path.display()
                )));
            }
            return Ok(false);
        }
        if !self.complete_gh_with_id(
            origin,
            Some(completion_id),
            verdict,
            evidence,
            completion,
            sink,
        )? {
            return Ok(false);
        }
        write_json_atomic(
            &marker_path,
            &GhCompletionMarker {
                completion_id: completion_id.to_owned(),
                producer: origin.producer.clone(),
                source: origin.source.clone(),
                item_id: origin.node_id.clone(),
            },
        )?;
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })?;
        Ok(true)
    }

    /// Post one durable, idempotent campaign-issue receipt for a daemon
    /// storage-budget episode. This deliberately does not reuse terminal
    /// completion policy: a warning must never close the issue, request review,
    /// or depend on failure-evidence publication.
    pub fn post_storage_warning_once(
        &self,
        origin: &GhOrigin,
        warning: &crate::storage::ActiveStorageWarning,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(&origin.producer)? else {
            return Err(self.kind_mismatch(&origin.producer, "gh"));
        };
        if !config.enable || config.never_mutate || !config.post_evidence {
            return Ok(false);
        }
        self.validate_gh_origin(origin)?;
        let completion_id = format!("storage-warning:{}", warning.warning_sequence);
        let completed_dir = self.state_dir.join("producers/gh-storage-warnings");
        create_dir_durable(&completed_dir)?;
        let lock_path = completed_dir.join("mutations.lock");
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let marker_key = stable_key(&[
            "gh-storage-warning",
            &origin.producer,
            &origin.source,
            &origin.node_id,
            &completion_id,
        ]);
        let marker_path = completed_dir.join(format!("{marker_key}.json"));
        if path_lexists(&marker_path)? {
            let marker: GhCompletionMarker =
                serde_json::from_slice(&read_bounded_regular(&marker_path, 64 * 1024)?)?;
            if marker.completion_id != completion_id
                || marker.producer != origin.producer
                || marker.source != origin.source
                || marker.item_id != origin.node_id
            {
                return Err(ProducerError::InvalidObservation(format!(
                    "GitHub storage-warning marker {} does not match its identity",
                    marker_path.display()
                )));
            }
            return Ok(false);
        }
        sink.post_evidence(&GhCompletedMutation {
            producer: origin.producer.clone(),
            source: origin.source.clone(),
            item_id: origin.node_id.clone(),
            completion_id: Some(completion_id.clone()),
            state: "COMPLETED".to_owned(),
            evidence: Some(serde_json::json!({
                "schemaVersion": 1,
                "kind": "storage-budget-warning",
                "warningSequence": warning.warning_sequence,
                "store": warning.store,
                "level": warning.level,
                "sizeBytes": warning.size_bytes,
                "thresholdBytes": warning.threshold_bytes,
                "message": warning.message,
                "intakePolicy": "new-intake-refused-at-hard-limit; admitted-work-continues",
            })),
            gate_summary: None,
            acceptance: None,
            request_review: false,
            assisted_by: None,
        })
        .map_err(ProducerError::Mutation)?;
        write_json_atomic(
            &marker_path,
            &GhCompletionMarker {
                completion_id,
                producer: origin.producer.clone(),
                source: origin.source.clone(),
                item_id: origin.node_id.clone(),
            },
        )?;
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })?;
        Ok(true)
    }

    fn complete_gh_with_id(
        &self,
        origin: &GhOrigin,
        completion_id: Option<&str>,
        verdict: Verdict,
        evidence: Option<Value>,
        completion: Option<SemanticCompletion>,
        sink: &mut dyn GhMutationSink,
    ) -> Result<bool, ProducerError> {
        let ProducerConfig::Gh(config) = self.get(&origin.producer)? else {
            return Err(self.kind_mismatch(&origin.producer, "gh"));
        };
        if !config.enable || config.never_mutate {
            return Ok(false);
        }
        self.validate_gh_origin(origin)?;
        let assisted_by = evidence.as_ref().and_then(assisted_by_from_evidence);
        let execution_passed = matches!(verdict, Verdict::Pass | Verdict::Reused);
        let execution_failed = matches!(
            verdict,
            Verdict::CleanExitNoArtifact
                | Verdict::Failed
                | Verdict::Cancelled
                | Verdict::PoolVanished
                | Verdict::RuntimeExceeded
        );
        // `postEvidence` predates failure receipts and remains a success-only
        // switch. Failure publication is deliberately separate because its
        // envelope originates in private process capture. Even an explicit
        // stderr opt-in passes through the conservative public redactor here,
        // at the last boundary before the mutation sink.
        let evidence = if execution_passed && config.post_evidence {
            public_evidence(evidence, false)
        } else if execution_failed && config.post_failure_evidence {
            public_evidence(evidence, config.post_failure_stderr)
        } else {
            None
        };
        let gate_summary = config
            .post_gate_summary
            .then(|| completion.as_ref().map(|facts| facts.gates.clone()))
            .flatten();
        let acceptance =
            (config.post_gate_summary || config.request_review || config.close_on_acceptance)
                .then(|| completion.as_ref().map(|facts| facts.acceptance.clone()))
                .flatten();
        let request_review = config.request_review
            && acceptance
                .as_ref()
                .is_none_or(|fact| fact.status != AcceptanceStatus::Accepted);
        let close_on_pass = config.close_on_pass()
            && execution_passed
            && completion
                .as_ref()
                .is_none_or(|facts| facts.gates.status == GateSummaryStatus::Pass);
        let close_on_acceptance = config.close_on_acceptance
            && completion
                .as_ref()
                .is_some_and(|facts| facts.acceptance.status == AcceptanceStatus::Accepted);
        let should_post =
            evidence.is_some() || gate_summary.is_some() || acceptance.is_some() || request_review;
        if !should_post && !close_on_pass && !close_on_acceptance {
            return Ok(false);
        }
        let mutation = GhCompletedMutation {
            producer: origin.producer.clone(),
            source: origin.source.clone(),
            item_id: origin.node_id.clone(),
            completion_id: completion_id.map(str::to_owned),
            state: "COMPLETED".to_owned(),
            evidence,
            gate_summary,
            acceptance,
            request_review,
            assisted_by,
        };
        if should_post {
            sink.post_evidence(&mutation)
                .map_err(ProducerError::Mutation)?;
        }
        if close_on_pass || close_on_acceptance {
            sink.close_item(&mutation)
                .map_err(ProducerError::Mutation)?;
        }
        Ok(true)
    }

    pub fn emit_build_effect(
        &self,
        producer: &str,
        store_path: &Path,
        now: DateTime<Utc>,
    ) -> Result<EmitOutcome, ProducerError> {
        let ProducerConfig::BuildEffect(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "build-effect"));
        };
        let store_path = validate_store_path(store_path)?;
        let dedup_key = format!("build-effect:{producer}:{store_path}");
        if read_acknowledged_events(&self.events_dir)?
            .iter()
            .any(|event| {
                event.row.source == EnqueueSource::BuildEffect
                    && event.row.dedup_key.as_deref() == Some(dedup_key.as_str())
            })
        {
            return Ok(EmitOutcome::Duplicate);
        }
        let mut payload =
            config
                .on_key
                .payload(EnqueueSource::BuildEffect, Some(producer), now, None)?;
        payload.dedup_key = Some(dedup_key);
        let key = stable_key(&["build-effect", producer, &store_path]);
        self.emit_named(
            &format!("{producer}-build-effect-{key}{INGRESS_SUFFIX}"),
            &payload,
        )
    }

    pub fn scan_build_effect(&self, producer: &str) -> Result<Vec<PathBuf>, ProducerError> {
        let ProducerConfig::BuildEffect(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "build-effect"));
        };
        scan_store_paths(config.watch, &config.path)
    }

    pub fn observe_reachability(
        &self,
        producer: &str,
        reachable: bool,
        now: DateTime<Utc>,
    ) -> Result<ReachabilityOutcome, ProducerError> {
        let ProducerConfig::PoolReachability(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "pool-reachability"));
        };
        let producer_state = self.state_dir.join("producers");
        create_dir_durable(&producer_state)?;
        let lock_path = producer_state.join(format!("{producer}.reachability.lock"));
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let state_path = producer_state.join(format!("{producer}.reachability.json"));
        let mut state = read_reachability_state(&state_path)?;
        match state.probe_pool.as_deref() {
            Some(bound) if bound == config.probe_pool => {}
            Some(bound) => {
                return Err(ProducerError::InvalidObservation(format!(
                    "reachability state {} is bound to probePool {bound:?}, not {:?}",
                    state_path.display(),
                    config.probe_pool
                )))
            }
            None if state.generation == 0 => {
                state.probe_pool = Some(config.probe_pool.clone());
            }
            None => {
                return Err(ProducerError::InvalidObservation(format!(
                    "reachability state {} has transitions without a probePool binding",
                    state_path.display()
                )))
            }
        }
        let mut transition = None;
        if state.generation == state.notified_generation {
            let expected_reachable = state.stable == ReachabilityStable::Reachable;
            if reachable == expected_reachable {
                state.candidate_reachable = None;
                state.consecutive = 0;
            } else {
                if state.candidate_reachable == Some(reachable) {
                    state.consecutive = state.consecutive.saturating_add(1);
                } else {
                    state.candidate_reachable = Some(reachable);
                    state.consecutive = 1;
                }
                if state.consecutive >= config.hysteresis {
                    state.stable = if reachable {
                        ReachabilityStable::Reachable
                    } else {
                        ReachabilityStable::Lost
                    };
                    state.candidate_reachable = None;
                    state.consecutive = 0;
                    state.generation = state.generation.checked_add(1).ok_or_else(|| {
                        ProducerError::InvalidObservation(
                            "reachability transition generation overflow".to_owned(),
                        )
                    })?;
                    transition = Some(if reachable {
                        ReachabilityTransition::Returned
                    } else {
                        ReachabilityTransition::Lost
                    });
                }
            }
        }

        let mut emitted = Vec::new();
        if let Some(active_transition) = transition {
            let actions: Vec<(&str, &ProducerEnqueue)> = match active_transition {
                ReachabilityTransition::Lost => config
                    .on_lost
                    .as_ref()
                    .map(|enqueue| vec![("lost", enqueue)])
                    .unwrap_or_default(),
                ReachabilityTransition::Returned => {
                    let mut actions = Vec::new();
                    if let Some(enqueue) = &config.on_return {
                        actions.push(("return", enqueue));
                    }
                    if let Some(enqueue) = &config.on_return_attest {
                        actions.push(("return-attest", enqueue));
                    }
                    actions
                }
            };
            for (action, enqueue) in actions {
                let payload =
                    enqueue.payload(EnqueueSource::PoolReachability, Some(producer), now, None)?;
                let name = format!(
                    "{producer}-reach-{}-{action}{INGRESS_SUFFIX}",
                    state.generation
                );
                if let EmitOutcome::Emitted(path) = self.emit_named(&name, &payload)? {
                    emitted.push(path);
                }
            }
        }
        let pending_transition =
            (state.generation > state.notified_generation).then_some(match state.stable {
                ReachabilityStable::Reachable => ReachabilityTransition::Returned,
                ReachabilityStable::Lost => ReachabilityTransition::Lost,
            });
        write_json_atomic(&state_path, &state)?;
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })?;
        Ok(ReachabilityOutcome {
            stable: state.stable,
            transition: pending_transition,
            generation: state.generation,
            emitted,
        })
    }

    pub fn validate_reachability_transition(
        &self,
        producer: &str,
        transition: ReachabilityTransition,
        generation: u64,
    ) -> Result<String, ProducerError> {
        let ProducerConfig::PoolReachability(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "pool-reachability"));
        };
        if generation == 0 {
            return Err(ProducerError::InvalidObservation(
                "reachability transition generation must be positive".to_owned(),
            ));
        }
        let state_path = self
            .state_dir
            .join("producers")
            .join(format!("{producer}.reachability.json"));
        let state = read_reachability_state(&state_path)?;
        validate_reachability_binding(&state, &state_path, &config.probe_pool)?;
        let expected = match state.stable {
            ReachabilityStable::Reachable => ReachabilityTransition::Returned,
            ReachabilityStable::Lost => ReachabilityTransition::Lost,
        };
        if state.generation != generation || expected != transition {
            return Err(ProducerError::InvalidObservation(format!(
                "reachability transition {transition:?} generation {generation} is not the current confirmed state for producer {producer:?}"
            )));
        }
        Ok(config.probe_pool.clone())
    }

    pub fn acknowledge_reachability_transition(
        &self,
        producer: &str,
        generation: u64,
    ) -> Result<(), ProducerError> {
        let ProducerConfig::PoolReachability(config) = self.get(producer)? else {
            return Err(self.kind_mismatch(producer, "pool-reachability"));
        };
        let producer_state = self.state_dir.join("producers");
        create_dir_durable(&producer_state)?;
        let lock_path = producer_state.join(format!("{producer}.reachability.lock"));
        let lock = open_private_rw(&lock_path)?;
        lock.lock_exclusive().map_err(|source| ProducerError::Io {
            path: lock_path.clone(),
            source,
        })?;
        let state_path = producer_state.join(format!("{producer}.reachability.json"));
        let mut state = read_reachability_state(&state_path)?;
        validate_reachability_binding(&state, &state_path, &config.probe_pool)?;
        if state.generation != generation {
            return Err(ProducerError::InvalidObservation(format!(
                "cannot acknowledge stale reachability generation {generation}; current generation is {}",
                state.generation
            )));
        }
        state.notified_generation = state.notified_generation.max(generation);
        write_json_atomic(&state_path, &state)?;
        FileExt::unlock(&lock).map_err(|source| ProducerError::Io {
            path: lock_path,
            source,
        })
    }

    pub fn confirmed_pool_returns(&self) -> Result<BTreeSet<String>, ProducerError> {
        let mut pools = BTreeSet::new();
        for (producer, config) in self.registry {
            let ProducerConfig::PoolReachability(config) = config else {
                continue;
            };
            let state_path = self
                .state_dir
                .join("producers")
                .join(format!("{producer}.reachability.json"));
            if !state_path.exists() {
                continue;
            }
            let state = read_reachability_state(&state_path)?;
            validate_reachability_binding(&state, &state_path, &config.probe_pool)?;
            if state.generation > 0 && state.stable == ReachabilityStable::Reachable {
                pools.insert(config.probe_pool.clone());
            }
        }
        Ok(pools)
    }

    pub fn confirmed_pool_losses(&self) -> Result<BTreeSet<String>, ProducerError> {
        let mut pools = BTreeSet::new();
        for (producer, config) in self.registry {
            let ProducerConfig::PoolReachability(config) = config else {
                continue;
            };
            let state_path = self
                .state_dir
                .join("producers")
                .join(format!("{producer}.reachability.json"));
            if !state_path.exists() {
                continue;
            }
            let state = read_reachability_state(&state_path)?;
            validate_reachability_binding(&state, &state_path, &config.probe_pool)?;
            if state.generation > 0 && state.stable == ReachabilityStable::Lost {
                pools.insert(config.probe_pool.clone());
            }
        }
        Ok(pools)
    }

    fn get(&self, producer: &str) -> Result<&ProducerConfig, ProducerError> {
        validate_producer_name(producer)?;
        self.registry
            .get(producer)
            .ok_or_else(|| ProducerError::UnknownProducer(producer.to_owned()))
    }

    fn kind_mismatch(&self, producer: &str, expected: &str) -> ProducerError {
        let actual = self
            .registry
            .get(producer)
            .map_or("unknown", ProducerConfig::kind);
        ProducerError::KindMismatch {
            producer: producer.to_owned(),
            expected: expected.to_owned(),
            actual: actual.to_owned(),
        }
    }

    fn emit_named(
        &self,
        name: &str,
        payload: &EnqueuePayload,
    ) -> Result<EmitOutcome, ProducerError> {
        // Retention takes the exclusive side of this lock before computing its
        // live set. Hold the shared side across both brief materialization and
        // ingress publication so it can observe either both or neither.
        let _brief_lock = payload
            .brief
            .as_ref()
            .map(|_| crate::brief::acquire_shared(&self.brief_root))
            .transpose()
            .map_err(|error| ProducerError::InvalidObservation(error.to_string()))?;
        let _ingress_lock = lock_ingress(&self.events_dir)?;
        if ingress_name_exists(&self.events_dir, name)? {
            return Ok(EmitOutcome::Duplicate);
        }
        let mut payload = payload.clone();
        if let Some(document) = payload.brief.take() {
            if payload.brief_path.is_some() {
                return Err(ProducerError::InvalidObservation(
                    "producer payload contains both brief and briefPath".to_owned(),
                ));
            }
            create_dir_durable(&self.brief_root)?;
            let prepared = crate::brief::PreparedBrief::from_value(document)
                .map_err(|error| ProducerError::InvalidObservation(error.to_string()))?;
            payload.brief_path = Some(
                crate::brief::store(&self.brief_root, &prepared)
                    .map_err(|error| ProducerError::InvalidObservation(error.to_string()))?,
            );
        }
        create_dir_durable(&self.events_dir)?;
        let bytes = serde_json::to_vec(&payload)?;
        if bytes.len().saturating_add(1) > MAX_INGRESS_BYTES as usize {
            return Err(ProducerError::InvalidObservation(format!(
                "producer payload exceeds the {MAX_INGRESS_BYTES} byte ingress limit"
            )));
        }
        let path = self.events_dir.join(name);
        if write_new_atomic(&path, &bytes)? {
            Ok(EmitOutcome::Emitted(path))
        } else {
            Ok(EmitOutcome::Duplicate)
        }
    }
}

const PUBLIC_STDERR_TRUNCATION_MARKER: &str = "[... earlier redacted stderr omitted ...]\n";
const PUBLIC_STDERR_REDACTION: &str = "conservative-v1";

fn public_evidence(evidence: Option<Value>, include_failure_stderr: bool) -> Option<Value> {
    evidence.map(|mut evidence| {
        let Value::Object(fields) = &mut evidence else {
            return evidence;
        };
        let stderr = fields.remove("stderrTail");
        let stderr_was_truncated = fields
            .remove("stderrTruncated")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        // Refuse alternate internal spellings rather than allowing a future
        // caller to bypass the one reviewed publication path.
        fields.remove("stderr_excerpt");
        fields.remove("stderr_truncated");
        if include_failure_stderr {
            if let Some(Value::String(stderr)) = stderr {
                let (stderr, redacted, additionally_truncated) = redact_public_stderr(&stderr);
                fields.insert("stderrTail".to_owned(), Value::String(stderr));
                fields.insert(
                    "stderrTruncated".to_owned(),
                    Value::Bool(stderr_was_truncated || additionally_truncated),
                );
                fields.insert(
                    "stderrRedaction".to_owned(),
                    Value::String(PUBLIC_STDERR_REDACTION.to_owned()),
                );
                fields.insert("stderrRedacted".to_owned(), Value::Bool(redacted));
            }
        }
        evidence
    })
}

fn redact_public_stderr(stderr: &str) -> (String, bool, bool) {
    let mut output = String::with_capacity(stderr.len());
    let mut redacted = false;
    let mut private_key_block = false;
    for line in stderr.split_inclusive('\n') {
        let lower = line.to_ascii_lowercase();
        if lower.contains("-----begin ") && lower.contains("private key-----") {
            private_key_block = true;
        }
        let sensitive_line = private_key_block || stderr_line_is_sensitive(&lower);
        if sensitive_line {
            output.push_str("[redacted sensitive stderr line]");
            if line.ends_with('\n') {
                output.push('\n');
            }
            redacted = true;
        } else {
            let (line, line_redacted) = redact_stderr_tokens(line);
            output.push_str(&line);
            redacted |= line_redacted;
        }
        if lower.contains("-----end ") && lower.contains("private key-----") {
            private_key_block = false;
        }
    }
    // `split_inclusive` yields no item for the empty string.
    if stderr.is_empty() {
        output.clear();
    }
    if output.len() <= crate::executor::CAPTURE_EXCERPT_MAX_BYTES {
        return (output, redacted, false);
    }
    let tail_limit =
        crate::executor::CAPTURE_EXCERPT_MAX_BYTES - PUBLIC_STDERR_TRUNCATION_MARKER.len();
    let mut start = output.len() - tail_limit;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    let mut bounded = String::with_capacity(crate::executor::CAPTURE_EXCERPT_MAX_BYTES);
    bounded.push_str(PUBLIC_STDERR_TRUNCATION_MARKER);
    bounded.push_str(&output[start..]);
    (bounded, redacted, true)
}

fn stderr_line_is_sensitive(lower: &str) -> bool {
    [
        "authorization",
        "bearer ",
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "api_key",
        "api-key",
        "apikey",
        "private key",
        "access key",
        "access_key",
        "secret_key",
        "client_secret",
        "client key",
        "cookie",
        "dsn=",
        "session_id",
        "sessionid",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn redact_stderr_tokens(line: &str) -> (String, bool) {
    let mut output = String::with_capacity(line.len());
    let mut redacted = false;
    for chunk in line.split_inclusive(char::is_whitespace) {
        let content_len = chunk.trim_end_matches(char::is_whitespace).len();
        let (content, spacing) = chunk.split_at(content_len);
        if stderr_token_is_sensitive(content) {
            output.push_str("[redacted-token]");
            redacted = true;
        } else {
            output.push_str(content);
        }
        output.push_str(spacing);
    }
    (output, redacted)
}

fn stderr_token_is_sensitive(token: &str) -> bool {
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
        )
    });
    let lower = token.to_ascii_lowercase();
    if [
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "sk-",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
    ]
    .iter()
    .any(|prefix| lower.contains(prefix))
        || ((token.contains("AKIA") || token.contains("ASIA")) && token.len() >= 16)
        || (token.contains("://") && (token.contains('@') || token.contains('?')))
    {
        return true;
    }
    let jwt_parts = token.split('.').collect::<Vec<_>>();
    if jwt_parts.len() == 3 && jwt_parts.iter().all(|part| part.len() >= 8) {
        return true;
    }
    if token.len() < 32 || !token.is_ascii() {
        return false;
    }
    let has_lower = token.bytes().any(|byte| byte.is_ascii_lowercase());
    let has_upper = token.bytes().any(|byte| byte.is_ascii_uppercase());
    let has_digit = token.bytes().any(|byte| byte.is_ascii_digit());
    if token.len() >= 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return true;
    }
    let token_like = token.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'_' | b'-' | b'=')
    });
    token_like && has_digit && ((has_lower && has_upper) || token.len() >= 40)
}

pub(super) fn stable_key(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

pub(super) fn validate_store_path(path: &Path) -> Result<String, ProducerError> {
    if !path.is_absolute() {
        return Err(ProducerError::InvalidObservation(format!(
            "build-effect store path {} is not absolute",
            path.display()
        )));
    }
    let components = path.components().collect::<Vec<_>>();
    if components.len() != 4
        || components[0] != Component::RootDir
        || components[1].as_os_str() != "nix"
        || components[2].as_os_str() != "store"
    {
        return Err(ProducerError::InvalidObservation(format!(
            "build-effect path {} is not one top-level /nix/store path",
            path.display()
        )));
    }
    let Some(name) = components[3].as_os_str().to_str() else {
        return Err(ProducerError::InvalidObservation(
            "build-effect store path must be valid UTF-8".to_owned(),
        ));
    };
    let Some((hash, output_name)) = name.split_once('-') else {
        return Err(ProducerError::InvalidObservation(format!(
            "build-effect store path {name:?} lacks a store hash"
        )));
    };
    if hash.len() != 32
        || !hash.bytes().all(|byte| {
            matches!(byte, b'0'..=b'9' | b'a'..=b'd' | b'f'..=b'n' | b'p'..=b's' | b'v'..=b'z')
        })
        || output_name.is_empty()
        || output_name.chars().any(char::is_control)
    {
        return Err(ProducerError::InvalidObservation(format!(
            "build-effect store path {name:?} has an invalid store name"
        )));
    }
    Ok(path.to_string_lossy().into_owned())
}

pub(super) fn scan_store_paths(
    watch: BuildEffectWatch,
    path: &Path,
) -> Result<Vec<PathBuf>, ProducerError> {
    let mut paths = BTreeSet::new();
    match watch {
        BuildEffectWatch::GcRootsDir => {
            let mut entries = std::fs::read_dir(path)
                .map_err(|source| ProducerError::Io {
                    path: path.to_owned(),
                    source,
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| ProducerError::Io {
                    path: path.to_owned(),
                    source,
                })?;
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let entry_path = entry.path();
                let metadata =
                    std::fs::symlink_metadata(&entry_path).map_err(|source| ProducerError::Io {
                        path: entry_path.clone(),
                        source,
                    })?;
                let candidate = if metadata.file_type().is_symlink() {
                    let target =
                        std::fs::read_link(&entry_path).map_err(|source| ProducerError::Io {
                            path: entry_path.clone(),
                            source,
                        })?;
                    if target.is_absolute() {
                        target
                    } else {
                        path.join(target)
                    }
                } else {
                    continue;
                };
                let normalized = validate_store_path(&candidate)?;
                paths.insert(PathBuf::from(normalized));
            }
        }
        BuildEffectWatch::Jsonl => {
            let bytes = read_bounded_regular(path, MAX_INGRESS_BYTES)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                ProducerError::InvalidObservation(format!(
                    "build-effect JSONL {} is not UTF-8",
                    path.display()
                ))
            })?;
            for (index, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(line).map_err(|error| {
                    ProducerError::InvalidObservation(format!(
                        "build-effect JSONL {} line {} is invalid: {error}",
                        path.display(),
                        index + 1
                    ))
                })?;
                collect_json_store_paths(&value, &mut paths)?;
            }
        }
        BuildEffectWatch::PostBuildHook => {
            let bytes = read_bounded_regular(path, MAX_INGRESS_BYTES)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                ProducerError::InvalidObservation(format!(
                    "build-effect post-build-hook stream {} is not UTF-8",
                    path.display()
                ))
            })?;
            for candidate in text.split_ascii_whitespace() {
                paths.insert(PathBuf::from(validate_store_path(Path::new(candidate))?));
            }
        }
    }
    Ok(paths.into_iter().collect())
}

pub(super) fn collect_json_store_paths(
    value: &Value,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<(), ProducerError> {
    let candidates = match value {
        Value::String(path) => vec![path.as_str()],
        Value::Object(object) => {
            if let Some(path) = object
                .get("storePath")
                .or_else(|| object.get("store_path"))
                .and_then(Value::as_str)
            {
                vec![path]
            } else if let Some(outputs) = object.get("outputs").and_then(Value::as_array) {
                outputs
                    .iter()
                    .map(|output| {
                        output.as_str().ok_or_else(|| {
                            ProducerError::InvalidObservation(
                                "build-effect JSONL outputs must contain only strings".to_owned(),
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                return Err(ProducerError::InvalidObservation(
                    "build-effect JSONL object requires storePath, store_path, or outputs"
                        .to_owned(),
                ));
            }
        }
        _ => {
            return Err(ProducerError::InvalidObservation(
                "build-effect JSONL entry must be a string or object".to_owned(),
            ))
        }
    };
    for candidate in candidates {
        paths.insert(PathBuf::from(validate_store_path(Path::new(candidate))?));
    }
    Ok(())
}
