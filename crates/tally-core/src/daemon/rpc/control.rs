use super::super::*;

impl DaemonHandler {
    pub(crate) async fn await_job(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            #[serde(default)]
            task_uuid: Option<String>,
            #[serde(default)]
            job_id: Option<String>,
            #[serde(default)]
            attempt: Option<u32>,
        }
        let params: Params = decode_params(params)?;
        if params.attempt == Some(0) {
            return Err(WireError::invalid("attempt must be positive"));
        }
        let requested_attempt = params.attempt;
        let presented = match (params.task_uuid, params.job_id) {
            (Some(task_uuid), None) => task_uuid,
            (None, Some(job_id)) => job_id,
            _ => {
                return Err(WireError::invalid(
                    "provide exactly one of task_uuid or job_id",
                ));
            }
        };
        let (registration, witness_lookup) = {
            let mut context = self.context.write().await;
            let job_id = context
                .aliases
                .get(&presented)
                .copied()
                .or_else(|| {
                    Uuid::parse_str(&presented).ok().filter(|uuid| {
                        context.jobs.contains_key(uuid) || context.rows.contains_key(uuid)
                    })
                })
                .ok_or_else(|| WireError::not_found(format!("job {presented} was not found")))?;
            let stable = context
                .jobs
                .get(&job_id)
                .map(Job::stable_key)
                .unwrap_or_else(|| job_id.to_string());
            let current = context.jobs.get(&job_id);
            let current_attempt = current
                .map(|job| job.row.attempt)
                .or_else(|| context.rows.get(&job_id).map(|row| row.attempt));
            let resolved_attempt = match (requested_attempt, current_attempt) {
                (Some(requested), Some(current)) if requested < current => Some(current),
                (Some(requested), _) => Some(requested),
                (None, current) => current,
            };
            if current.is_some_and(|job| {
                job.state != JobState::Completed
                    && resolved_attempt.is_none_or(|attempt| job.row.attempt == attempt)
            }) {
                (Some(context.barriers.wait_job(&stable)), None)
            } else {
                (
                    None,
                    Some(terminal_witness(&mut context, &stable, resolved_attempt)),
                )
            }
        };
        if let Some(registration) = registration {
            return await_registration(registration).await;
        }
        project_job_result(
            witness_lookup.expect("terminal witness lookup was selected above"),
            self.executor.clone(),
        )
        .await
    }

    pub(crate) async fn await_barrier(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            barrier: String,
        }
        let params: Params = decode_params(params)?;
        if params.barrier.starts_with("barrier:drain:") {
            let registration = self
                .context
                .write()
                .await
                .barriers
                .wait_barrier(&params.barrier)?;
            return await_registration(registration).await;
        }
        let (presented, attempt) = parse_job_barrier(&params.barrier)?;
        let (registration, witness_lookup, stable) = {
            let mut context = self.context.write().await;
            let job_id = context
                .aliases
                .get(presented)
                .copied()
                .or_else(|| {
                    Uuid::parse_str(presented).ok().filter(|uuid| {
                        context.jobs.contains_key(uuid) || context.rows.contains_key(uuid)
                    })
                })
                .ok_or_else(|| WireError::not_found(format!("job {presented} was not found")))?;
            let stable = context
                .jobs
                .get(&job_id)
                .map(Job::stable_key)
                .unwrap_or_else(|| job_id.to_string());
            if stable != presented {
                return Err(WireError::not_found(format!(
                    "barrier {} does not identify job {stable}",
                    params.barrier
                )));
            }
            if context
                .jobs
                .get(&job_id)
                .is_some_and(|job| job.state != JobState::Completed && job.row.attempt == attempt)
            {
                (Some(context.barriers.wait_job(&stable)), None, stable)
            } else {
                (
                    None,
                    Some(terminal_witness(&mut context, &stable, Some(attempt))),
                    stable.clone(),
                )
            }
        };
        let result = if let Some(registration) = registration {
            await_registration(registration).await?
        } else {
            project_job_result(
                witness_lookup.expect("terminal barrier lookup was selected above"),
                self.executor.clone(),
            )
            .await?
        };
        Ok(single_job_barrier_value(&params.barrier, &stable, result))
    }

    pub(crate) async fn drain(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Params {
            #[serde(default)]
            producer: Option<String>,
        }
        let params: Params = decode_params(params)?;
        if let Some(producer) = &params.producer {
            let context = self.context.read().await;
            let configured = context
                .config
                .producers
                .get(producer)
                .ok_or_else(|| WireError::invalid(format!("unknown producer {producer:?}")))?;
            if configured.kind() != "events-dir" {
                return Err(WireError::invalid(format!(
                    "producer {producer:?} is not an events-dir producer"
                )));
            }
        }
        let _sweep = self.ingress_sweep.lock().await;
        let events_dir = self.context.read().await.paths.events_dir();
        let claims = claim_ingress_files(&events_dir).map_err(internal_wire)?;
        let acknowledged = acknowledged_ingress_ids(&events_dir).map_err(internal_wire)?;
        let mut outcomes = Vec::with_capacity(claims.len());
        let mut enqueued = 0_u64;
        let mut rejected = 0_u64;
        let mut repaired = 0_u64;
        for claim in claims {
            if acknowledged.contains(&claim.ingress_id) {
                let archived_to =
                    archive_ingress_claim(&events_dir, &claim, true).map_err(internal_wire)?;
                repaired = repaired.saturating_add(1);
                outcomes.push(IngressOutcome {
                    file: claim.original_name,
                    status: "accepted".to_owned(),
                    archived_to: Some(archived_to),
                    reason: Some("repaired acknowledged archive after interruption".to_owned()),
                });
                continue;
            }
            let mut payload = match read_ingress_payload(&claim) {
                Ok(payload) => payload,
                Err(error @ crate::producers::ProducerError::Io { .. }) => {
                    return Err(internal_wire(error));
                }
                Err(error) => {
                    let reason = format!("invalid enqueue params: {error}");
                    let archived_to =
                        archive_ingress_claim(&events_dir, &claim, false).map_err(internal_wire)?;
                    eprintln!(
                        "tally: rejected producer ingress {}: {reason}",
                        claim.original_name
                    );
                    rejected = rejected.saturating_add(1);
                    outcomes.push(IngressOutcome {
                        file: claim.original_name,
                        status: "rejected".to_owned(),
                        archived_to: Some(archived_to),
                        reason: Some(reason),
                    });
                    continue;
                }
            };
            if payload.origin.is_none() {
                if payload.source.is_none() && params.producer.is_some() {
                    payload.source = Some(EnqueueSource::EventsDir);
                }
                if payload.source == Some(EnqueueSource::EventsDir) {
                    payload.origin = Some(params.producer.as_ref().map_or_else(
                        || AdmissionOrigin::direct(EnqueueSource::EventsDir),
                        |producer| AdmissionOrigin::producer(producer, EnqueueSource::EventsDir),
                    ));
                }
            }
            match self
                .enqueue_payload(
                    payload,
                    Some(claim.ingress_id.clone()),
                    CallerIdentity::Client,
                )
                .await
            {
                Ok(_) => {
                    let archived_to =
                        archive_ingress_claim(&events_dir, &claim, true).map_err(internal_wire)?;
                    enqueued = enqueued.saturating_add(1);
                    outcomes.push(IngressOutcome {
                        file: claim.original_name,
                        status: "accepted".to_owned(),
                        archived_to: Some(archived_to),
                        reason: None,
                    });
                }
                Err(error)
                    if matches!(
                        error.code,
                        WireErrorCode::InvalidParams | WireErrorCode::NotFound
                    ) =>
                {
                    let reason = format!("enqueue failed: {}", error.message);
                    let archived_to =
                        archive_ingress_claim(&events_dir, &claim, false).map_err(internal_wire)?;
                    eprintln!(
                        "tally: rejected producer ingress {}: {reason}",
                        claim.original_name
                    );
                    rejected = rejected.saturating_add(1);
                    outcomes.push(IngressOutcome {
                        file: claim.original_name,
                        status: "rejected".to_owned(),
                        archived_to: Some(archived_to),
                        reason: Some(reason),
                    });
                }
                Err(error) => return Err(error),
            }
        }
        let mut context = self.context.write().await;
        let active = context
            .jobs
            .values()
            .filter(|job| job.state != JobState::Completed)
            .map(Job::stable_key)
            .collect::<Vec<_>>();
        let barrier = context.barriers.snapshot(active);
        Ok(json!({
            "barrier": barrier,
            "enqueued": enqueued,
            "rejected": rejected,
            "repaired": repaired,
            "represented": 0,
            "outcomes": outcomes,
        }))
    }

    pub(crate) async fn pause(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            #[serde(default)]
            pool: Option<String>,
            #[serde(default)]
            all: bool,
        }
        let params: Params = decode_params(params)?;
        let mut context = self.context.write().await;
        let pools = selected_pools(&context.config, params.pool, params.all)?;
        for pool in &pools {
            context.paused_pools.insert(pool.clone());
        }
        let queued = context
            .jobs
            .values()
            .filter(|job| {
                job.state == JobState::Queued
                    && job.row.pools.iter().any(|pool| pools.contains(pool))
            })
            .map(|job| (job.job_id, job.lease_id.clone()))
            .collect::<Vec<_>>();
        for (job_id, lease_id) in &queued {
            if let Some(lease_id) = lease_id {
                let epoch = context.epoch;
                context
                    .lease
                    .engine_mut()
                    .cancel_pending_at(lease_id, epoch, Utc::now())
                    .map_err(lease_wire)?;
                context.lease_jobs.remove(lease_id);
            }
            let job = context.jobs.get_mut(job_id).expect("queued job exists");
            job.lease_id = None;
            job.state = JobState::Paused;
        }
        let affected = queued.len();
        drop(context);
        for pool in &pools {
            self.append_change(
                ChangeKind::Pool,
                json!({"pool": pool, "update": "paused", "affected": affected}),
            )?;
        }
        Ok(json!({"paused": pools, "affected": affected}))
    }

    pub(crate) async fn resume(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            #[serde(default)]
            pool: Option<String>,
            #[serde(default)]
            all: bool,
        }
        let params: Params = decode_params(params)?;
        let mut context = self.context.write().await;
        let pools = selected_pools(&context.config, params.pool, params.all)?;
        for pool in &pools {
            context.paused_pools.remove(pool);
        }
        let paused_jobs = context
            .jobs
            .values()
            .filter(|job| {
                job.state == JobState::Paused
                    && job.row.pools.iter().any(|pool| pools.contains(pool))
                    && !job.row.pools.iter().any(|pool| {
                        context.paused_pools.contains(pool)
                            || context.unreachable_pools.contains(pool)
                    })
            })
            .map(|job| job.job_id)
            .collect::<Vec<_>>();
        for job_id in &paused_jobs {
            context.unreachable_paused_jobs.remove(job_id);
        }
        let launches = resume_paused_jobs_locked(&mut context, &self.executor, paused_jobs);
        drop(context);
        for job in launches {
            self.spawn_execution(job);
        }
        for pool in &pools {
            self.append_change(ChangeKind::Pool, json!({"pool": pool, "update": "resumed"}))?;
        }
        Ok(json!({"resumed": pools}))
    }

    pub(crate) async fn cancel(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            #[serde(default)]
            task_uuid: Option<String>,
            #[serde(default, alias = "flowRunId")]
            flow_run_id: Option<String>,
            #[serde(default)]
            force: bool,
        }
        let params: Params = decode_params(params)?;
        match (params.task_uuid, params.flow_run_id) {
            (Some(task_uuid), None) => self.cancel_one(&task_uuid, params.force).await,
            (None, Some(flow_run_id)) => self.cancel_flow(&flow_run_id).await,
            _ => Err(WireError::invalid(
                "provide exactly one of task_uuid or flow_run_id",
            )),
        }
    }

    pub(crate) async fn cancel_flow(&self, flow_run_id: &str) -> Result<Value, WireError> {
        Uuid::parse_str(flow_run_id)
            .map_err(|_| WireError::invalid("flow_run_id must be a UUID"))?;
        let mut task_uuids = {
            let context = self.context.read().await;
            context
                .jobs
                .values()
                .filter(|job| job.state != JobState::Completed)
                .filter(|job| job_is_in_flow_run(job, flow_run_id))
                .map(Job::stable_key)
                .collect::<Vec<_>>()
        };
        task_uuids.sort();
        let mut affected = 0_u64;
        let mut results = Vec::with_capacity(task_uuids.len());
        for task_uuid in task_uuids {
            let result = self.cancel_one(&task_uuid, true).await?;
            affected = affected
                .saturating_add(result.get("affected").and_then(Value::as_u64).unwrap_or(0));
            results.push(result);
        }
        Ok(json!({
            "ok": true,
            "affected": affected,
            "flow_run_id": flow_run_id,
            "flowRunId": flow_run_id,
            "results": results,
        }))
    }

    /// Record the durable transition from a terminal generation to its successor.
    ///
    /// Fatal replay divergence is a correct refusal, but a refusal alone is not
    /// a recovery: a supervisor that persists one `flowRunId` per work item can
    /// only re-observe it forever. This operation is the machine-actionable half
    /// — it preserves the old run untouched, names the successor durably, and is
    /// safe to call again after the supervisor's own restart.
    pub(crate) async fn supersede_flow(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Params {
            #[serde(alias = "flow_run_id")]
            flow_run_id: String,
            #[serde(alias = "newFlowRunId", alias = "new_flow_run_id")]
            successor_flow_run_id: String,
            reason: SupersedeReason,
        }
        let params: Params = decode_params(params)?;
        // Canonicalize before anything else. A run ID may be presented upper
        // case, unhyphenated, or braced and still parse; storing the caller's
        // spelling would key the rollover to a run nobody ever replays, and
        // answer `ok: true` while recovering nothing.
        let predecessor = canonical_flow_run_id(&params.flow_run_id)
            .map_err(|_| WireError::invalid("flowRunId must be a UUID"))?;
        let successor = canonical_flow_run_id(&params.successor_flow_run_id)
            .map_err(|_| WireError::invalid("successorFlowRunId must be a UUID"))?;
        let path = self.context.read().await.paths.flow_lineage_path();
        let already_recorded = self
            .flow_lineage()
            .await?
            .classify(&predecessor, &successor, params.reason)
            .map_err(lineage_wire)?
            .is_some();
        // The idempotent retry answers from the durable record before any
        // liveness question, so a supervisor that crashed between recording the
        // rollover and running the successor can always call again.
        let pins = if already_recorded {
            PredecessorPins::default()
        } else {
            let pins = self.assert_supersedable(&predecessor, &successor).await?;
            pins
        };
        let outcome = record_supersede(&path, &predecessor, &successor, params.reason, &pins)
            .map_err(lineage_wire)?;
        self.invalidate_flow_lineage().await;
        serde_json::to_value(outcome).map_err(internal_wire)
    }

    /// Refuse a rollover that names no real run, would strand live work, or
    /// would continue an already started successor; return the predecessor's
    /// own pinned hashes on success.
    ///
    /// Existence and freshness are asked of the durable row projection rather
    /// than of live jobs: a run started under an earlier daemon epoch is still a
    /// started run, and the same projection is what the runner's own identity
    /// scan reads.
    async fn assert_supersedable(
        &self,
        predecessor: &str,
        successor: &str,
    ) -> Result<PredecessorPins, WireError> {
        let (unfinished, details) = {
            let mut context = self.context.write().await;
            let unfinished = context
                .jobs
                .values()
                .filter(|job| job.state != JobState::Completed)
                .filter(|job| job_is_in_flow_run(job, predecessor))
                .count();
            (unfinished, context.query_details.snapshot())
        };
        if unfinished > 0 {
            return Err(WireError::new(
                WireErrorCode::FlowLineageConflict,
                format!(
                    "flow run {predecessor} still has {unfinished} unfinished node(s); \
                     cancel the run before superseding it"
                ),
            ));
        }
        let started = flow_run_details(&details, successor).count();
        if started > 0 {
            return Err(WireError::new(
                WireErrorCode::FlowLineageConflict,
                format!(
                    "successor flow run {successor} already has {started} node(s); \
                     a successor starts fresh"
                ),
            ));
        }
        // A run with no durable node never recorded a script hash, so it can
        // never trip a startup identity pin and never needs superseding. Every
        // other run-keyed verb answers not-found for it; this one used to
        // answer `ok: true`, which is the silent no-op #251 is about.
        predecessor_pins(&details, predecessor)
    }

    /// The parsed lineage ledger, re-read only when its bytes changed.
    ///
    /// Every flow start asks `query.lineage`, so an uncached read would parse
    /// the whole ledger once per run — linear in a store that grows by one
    /// record per retired generation. The daemon caches the parsed index and
    /// revalidates it against the file's length and modification time, so an
    /// external edit is still picked up.
    pub(crate) async fn flow_lineage(&self) -> Result<Rc<FlowLineage>, WireError> {
        let path = self.context.read().await.paths.flow_lineage_path();
        let stamp = std::fs::metadata(&path)
            .ok()
            .map(|metadata| (metadata.len(), metadata.modified().ok()));
        if let Some(cached) = self.flow_lineage_cache.borrow().as_ref() {
            if cached.stamp == stamp {
                return Ok(cached.lineage.clone());
            }
        }
        let lineage = Rc::new(FlowLineage::read(&path).map_err(lineage_wire)?);
        *self.flow_lineage_cache.borrow_mut() = Some(CachedFlowLineage {
            stamp,
            lineage: lineage.clone(),
        });
        Ok(lineage)
    }

    /// Drop the cached lineage after this daemon wrote the ledger itself.
    pub(crate) async fn invalidate_flow_lineage(&self) {
        self.flow_lineage_cache.borrow_mut().take();
    }

    /// The parsed run-membership ledger, re-read only when its bytes changed.
    pub(crate) async fn flow_membership(&self) -> Result<Rc<FlowMembership>, WireError> {
        let path = self.context.read().await.paths.flow_membership_path();
        let stamp = membership_stamp(&path);
        if let Some(cached) = self.flow_membership_cache.borrow().as_ref() {
            if cached.stamp == stamp {
                return Ok(cached.membership.clone());
            }
        }
        let membership = Rc::new(FlowMembership::read(&path).map_err(membership_wire)?);
        *self.flow_membership_cache.borrow_mut() = Some(CachedFlowMembership {
            stamp,
            membership: membership.clone(),
        });
        Ok(membership)
    }

    /// Prove the membership ledger is usable *before* the kernel commits.
    ///
    /// A flow admission cannot record membership until the kernel has decided
    /// which task UUID the run is being handed, and by then the row, the
    /// `enqueued` journal event, and the dispatcher registration are already
    /// durable. Failing at that point and reporting an error to the caller is
    /// not a refusal — it orphans live work. So the two faults that are actually
    /// reachable, an unusable record and a ledger that cannot be opened for
    /// append, are detected here, where returning an error refuses an admission
    /// that has not happened yet.
    pub(crate) async fn preflight_flow_membership(&self) -> Result<(), WireError> {
        let path = self.context.read().await.paths.flow_membership_path();
        let stamp = membership_stamp(&path);
        if self
            .flow_membership_cache
            .borrow()
            .as_ref()
            .is_some_and(|cached| cached.stamp == stamp)
        {
            // The cache is current, so the ledger already parsed cleanly as of
            // these bytes and re-parsing would be the very per-admission linear
            // cost the cache exists to avoid. Only the appendability check is
            // left, and it is one open.
            return crate::flow_membership::probe_appendable(&path).map_err(membership_wire);
        }
        let membership =
            Rc::new(crate::flow_membership::preflight(&path).map_err(membership_wire)?);
        *self.flow_membership_cache.borrow_mut() = Some(CachedFlowMembership {
            stamp: membership_stamp(&path),
            membership,
        });
        Ok(())
    }

    /// Make `flow_run_id` durably hold `task_uuid`.
    ///
    /// The whole point of the record is that it is written for the dispositions
    /// that write no row of their own: an `attached` caller handed a task UUID
    /// it will never be able to see in its own window is W-316 all over again.
    /// So this must not fail silently — but by the time it runs the admission
    /// has already happened, so it must not fail *loudly to the caller* either.
    /// [`Self::preflight_flow_membership`] takes the reachable faults before the
    /// commit; what is left here is the narrow race where the ledger became
    /// unusable in between, and the caller gets a degraded acknowledgement
    /// rather than a false refusal. The error is returned for the admission path
    /// to attach, never for it to propagate.
    pub(crate) async fn record_flow_membership(
        &self,
        record: FlowMembershipRecord,
    ) -> Result<(), WireError> {
        let path = self.context.read().await.paths.flow_membership_path();
        // Scoped so the borrowed index is dropped before the cache is taken:
        // holding it here would keep the `Rc` strong count at two, `try_unwrap`
        // below would fail, and every admission would deep-clone the whole
        // index — linear in the ledger, which is the cost this repair removes.
        let already_held = {
            let held = self.flow_membership().await?;
            held.contains(&record.flow_run_id, &record.task_uuid)
        };
        if already_held {
            return Ok(());
        }
        let owned = match self.flow_membership_cache.borrow_mut().take() {
            Some(cached) => {
                Rc::try_unwrap(cached.membership).unwrap_or_else(|shared| (*shared).clone())
            }
            // Only reachable if a concurrent reader invalidated it in between.
            None => FlowMembership::read(&path).map_err(membership_wire)?,
        };
        match record_membership(&path, &record, owned) {
            Ok((_, updated)) => {
                // The cache is whatever the writer says the file now holds --
                // including after a compaction, which is what stops the cache
                // from staying permanently over the bound and rewriting the
                // whole ledger on every later admission.
                *self.flow_membership_cache.borrow_mut() = Some(CachedFlowMembership {
                    stamp: membership_stamp(&path),
                    membership: Rc::new(updated),
                });
                Ok(())
            }
            Err(error) => {
                // The index went with the failed write; leave the cache empty so
                // the next reader re-parses whatever survived on disk rather
                // than trusting a projection of a write that did not land.
                Err(membership_wire(error))
            }
        }
    }

    pub(crate) async fn cancel_one(
        &self,
        task_uuid: &str,
        force: bool,
    ) -> Result<Value, WireError> {
        let mut context = self.context.write().await;
        let job = find_job(&context, task_uuid)?.clone();
        let was = state_name(job.state);
        if job.state == JobState::Completed {
            let mut response = json!({
                "ok": true,
                "affected": 0,
                "task_uuid": job.task_uuid.map(|uuid| uuid.to_string()),
                "was": was,
                "lease_epoch": job.row.lease_epoch,
                "already_terminal": true,
            });
            insert_job_task_ref(&mut response, &job);
            return Ok(response);
        }
        if job.state == JobState::Running && !force {
            let mut response = json!({
                "ok": true,
                "affected": 0,
                "task_uuid": job.task_uuid.map(|uuid| uuid.to_string()),
                "was": was,
                "lease_epoch": job.row.lease_epoch,
            });
            insert_job_task_ref(&mut response, &job);
            return Ok(response);
        }
        let scrape_capture = if job.state == JobState::Running {
            let identity = job.identity();
            if let Err(error) = self
                .executor
                .reclaim_identity_exact_on(
                    job.row.executor.as_deref(),
                    &identity,
                    job.adopted_invocation_id.as_deref(),
                    job.row.attempt,
                    job.row.lease_epoch,
                )
                .await
            {
                return Err(internal_wire(error.to_string()));
            }
            let _ = self.execution_cancel.send(job.job_id);
            match self.executor.capture_generation_matches(
                &identity,
                job.row.attempt,
                job.row.lease_epoch,
            ) {
                Ok(matches) => matches,
                Err(error) => {
                    eprintln!(
                        "tally: cancelled job {} capture generation is unavailable: {error}",
                        job.stable_key()
                    );
                    false
                }
            }
        } else {
            false
        };
        let work = match finalize_forced_locked(
            &mut context,
            job.job_id,
            Verdict::Cancelled,
            true,
            scrape_capture,
        ) {
            Ok(work) => work,
            Err(error) => return Err(self.fail_stop(error)),
        };
        drop(context);
        if let Some(work) = work {
            self.complete_terminal_post_ack(work);
        }
        let mut response = json!({
            "ok": true,
            "affected": 1,
            "task_uuid": job.task_uuid.map(|uuid| uuid.to_string()),
            "was": was,
            "lease_epoch": job.row.lease_epoch,
        });
        insert_job_task_ref(&mut response, &job);
        Ok(response)
    }

    pub(crate) async fn acquire(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            #[serde(deserialize_with = "crate::poolset::deserialize")]
            pool: Vec<String>,
        }
        let mut params: Params = decode_params(params)?;
        crate::poolset::canonicalize(&mut params.pool)
            .map_err(|error| WireError::invalid(error.to_string()))?;
        let id = Uuid::new_v4();
        let mut context = self.context.write().await;
        let epoch = context.epoch;
        let outcome = match context.lease.admit(
            LeaseRequest {
                job_id: id.to_string(),
                unit: format!("tally-job-{id}.service"),
                pools: params.pool,
                // The additive acquire/release surface is an explicit
                // reservation token, not a daemon-owned execution. Keep
                // it outside managed hard-preemption; only daemon jobs
                // have a unit identity that tally is authorized to stop.
                priority: Priority::Interrupt,
                admission_key: None,
                consumption_estimate: None,
                scheduling_group: LeaseSchedulingGroup::Standalone,
            },
            Utc::now(),
        ) {
            Ok(outcome) => outcome,
            Err(
                error @ (LeaseError::UnknownPool(_)
                | LeaseError::InvalidRequest(_)
                | LeaseError::StaleEpoch { .. }
                | LeaseError::NotFound(_)),
            ) => return Err(lease_wire(error)),
            Err(error) => return Err(self.fail_stop(error.into())),
        };
        Ok(json!({"epoch": epoch, "outcome": outcome}))
    }

    pub(crate) async fn release(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        struct Params {
            lease: String,
        }
        let params: Params = decode_params(params)?;
        let mut context = self.context.write().await;
        let epoch = context.epoch;
        let outcome = match context.lease.release(&params.lease, epoch, Utc::now()) {
            Ok(outcome) => outcome,
            Err(
                error @ (LeaseError::UnknownPool(_)
                | LeaseError::InvalidRequest(_)
                | LeaseError::StaleEpoch { .. }
                | LeaseError::NotFound(_)),
            ) => return Err(lease_wire(error)),
            Err(error) => return Err(self.fail_stop(error.into())),
        };
        let promoted = outcome.promoted.clone();
        let launches = promoted_jobs(&mut context, outcome.promoted);
        drop(context);
        for job in launches {
            self.spawn_execution(job);
        }
        Ok(json!({"released": outcome.released, "promoted": promoted}))
    }

    pub(crate) async fn lease_status(&self, params: Option<Value>) -> Result<Value, WireError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct Params {
            #[serde(default)]
            lease: Option<String>,
            #[serde(default)]
            job_id: Option<String>,
        }
        let params: Params = decode_params(params)?;
        let context = self.context.read().await;
        let lease = match (params.lease, params.job_id) {
            (Some(lease), None) if !lease.trim().is_empty() => lease,
            (None, Some(job_id)) if !job_id.trim().is_empty() => {
                let job = find_job(&context, &job_id)?;
                job.lease_id.clone().ok_or_else(|| {
                    WireError::not_found(format!("job {job_id} has no active lease"))
                })?
            }
            _ => {
                return Err(WireError::invalid(
                    "lease status requires exactly one non-empty lease or jobId",
                ))
            }
        };
        let status = context
            .lease
            .engine()
            .status(&lease, context.epoch)
            .map_err(lease_wire)?;
        serde_json::to_value(status).map_err(|error| internal_wire(error.to_string()))
    }
}

fn flow_run_details<'a>(
    details: &'a [RowDetailFact],
    flow_run_id: &'a str,
) -> impl Iterator<Item = &'a RowDetailFact> {
    details.iter().filter(move |detail| {
        detail
            .orchestration
            .as_ref()
            .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run_id)
    })
}

pub(crate) fn job_is_in_flow_run(job: &Job, flow_run_id: &str) -> bool {
    job.row
        .orchestration
        .as_ref()
        .is_some_and(|orchestration| orchestration.flow_run_id() == flow_run_id)
}

/// The abandoned generation's own pinned hashes, read from its durable rows.
///
/// Never taken from the caller: this is the frozen fingerprint of what was
/// abandoned, and it is what makes the boundary auditable later.
///
/// A predecessor with no durable node, or with no recorded `scriptHash`, is not
/// a flow run this operation can retire — it can never trip a startup identity
/// pin, so it can never need retiring — and is refused as not found rather than
/// recorded with the hashes silently omitted. Rows that disagree about a pin are
/// the `*-history-conflict` pathology and are refused too: recording an
/// arbitrary one of two hashes would put a lie in the audit record.
fn predecessor_pins(
    details: &[RowDetailFact],
    predecessor: &str,
) -> Result<PredecessorPins, WireError> {
    let mut script = BTreeSet::new();
    let mut args = BTreeSet::new();
    let mut catalog = BTreeSet::new();
    let mut rows = 0_usize;
    for orchestration in
        flow_run_details(details, predecessor).filter_map(|detail| detail.orchestration.as_ref())
    {
        rows += 1;
        for (field, sink) in [
            ("scriptHash", &mut script),
            ("argsHash", &mut args),
            ("catalogHash", &mut catalog),
        ] {
            if let Some(hash) = orchestration.as_value().get(field).and_then(Value::as_str) {
                sink.insert(hash.to_owned());
            }
        }
    }
    if rows == 0 {
        return Err(WireError::not_found(format!(
            "flow run {predecessor} has no durable node; there is no generation to supersede"
        )));
    }
    for (name, values) in [
        ("scriptHash", &script),
        ("argsHash", &args),
        ("catalogHash", &catalog),
    ] {
        if values.len() > 1 {
            return Err(WireError::new(
                WireErrorCode::FlowLineageConflict,
                format!(
                    "flow run {predecessor} recorded {} different {name} values; \
                     resolve that history conflict before superseding it",
                    values.len()
                ),
            ));
        }
    }
    if script.is_empty() {
        return Err(WireError::not_found(format!(
            "flow run {predecessor} recorded no orchestration scriptHash; it is not a flow run"
        )));
    }
    let single = |values: BTreeSet<String>| values.into_iter().next();
    Ok(PredecessorPins {
        script_hash: single(script),
        args_hash: single(args),
        catalog_hash: single(catalog),
    })
}

pub(crate) fn lineage_wire(error: FlowLineageError) -> WireError {
    match error {
        FlowLineageError::Invalid(message) => WireError::invalid(message),
        FlowLineageError::Conflict(message) => {
            WireError::new(WireErrorCode::FlowLineageConflict, message)
        }
        // A ledger that cannot be read stops every flow start in the estate, so
        // it must never look like an anonymous internal fault to a supervisor:
        // it is permanent, and the operator action is bounded and named.
        other => WireError {
            code: WireErrorCode::FlowLineageUnusable,
            message: other.to_string(),
            data: Some(json!({
                "transient": false,
                "resolution": "repair-lineage-ledger",
            })),
        },
    }
}

fn membership_stamp(path: &Path) -> Option<(u64, Option<std::time::SystemTime>)> {
    std::fs::metadata(path)
        .ok()
        .map(|metadata| (metadata.len(), metadata.modified().ok()))
}

/// A membership ledger that cannot be read or written is not an anonymous
/// internal fault: it makes every run-scoped window under-report, which is the
/// one direction an observability surface must never fail in. Name it, and name
/// the bounded repair.
pub(crate) fn membership_wire(error: FlowMembershipError) -> WireError {
    match error {
        FlowMembershipError::Invalid(message) => WireError::invalid(message),
        other => WireError {
            code: WireErrorCode::Internal,
            message: other.to_string(),
            data: Some(json!({
                "transient": false,
                "resolution": "repair-flow-membership-ledger",
            })),
        },
    }
}

pub(crate) fn lease_wire(error: LeaseError) -> WireError {
    match error {
        LeaseError::UnknownPool(_)
        | LeaseError::InvalidRequest(_)
        | LeaseError::StaleEpoch { .. } => WireError::invalid(error.to_string()),
        LeaseError::NotFound(_) => WireError::not_found(error.to_string()),
        other => internal_wire(other),
    }
}

/// Resolve the terminal witness a durable wait must answer from.
///
/// Deliberately stops at the record. Projecting it into a `JobResult` reads
/// retained capture files and takes the per-unit capture lock, and this runs
/// under the daemon's context write lock on the single-threaded RPC runtime —
/// so the file work is handed to [`project_job_result`] once that lock is
/// released.
fn terminal_witness(
    context: &mut Context,
    stable: &str,
    attempt: Option<u32>,
) -> Result<WitnessRecord, WireError> {
    context
        .witness_view
        .latest_for_task(stable, attempt)
        .map_err(internal_wire)?
        .ok_or_else(|| {
            let suffix = attempt.map_or_else(String::new, |attempt| format!(" attempt {attempt}"));
            WireError::not_found(format!("job {stable}{suffix} has no terminal witness"))
        })
}

/// Project a terminal witness into a wire `JobResult` off the async runtime.
///
/// `job_result_from_witness` materializes the failure-stderr projection, which
/// opens files and blocks on `flock`. Neither belongs on an async worker thread,
/// and the callers below have already dropped the context write lock.
async fn project_job_result(
    record: Result<WitnessRecord, WireError>,
    executor: Executor,
) -> Result<Value, WireError> {
    let record = record?;
    tokio::task::spawn_blocking(move || job_result_from_witness(&record, &executor).value())
        .await
        .map_err(|error| internal_wire(error.to_string()))
}

fn job_result_from_witness(record: &WitnessRecord, executor: &Executor) -> JobResult {
    let stable = record
        .task_uuid
        .clone()
        .expect("durable wait reconstruction selected a task witness");
    let stderr_excerpt = retained_failure_stderr_excerpt(record, executor);
    JobResult {
        task_uuid: Some(stable.clone()),
        task_ref: record
            .orchestration
            .as_ref()
            .and_then(Orchestration::task_ref),
        job_id: stable,
        verdict: record.verdict,
        exit_code: record.exit_code,
        artifact_content_hash: record.artifact_content_hash.clone(),
        attempt: record.attempt,
        lease_epoch: record.lease_epoch,
        witness_seq: record.seq,
        model: record.model.clone(),
        completion: record.completion.clone(),
        stderr_excerpt,
    }
}

fn selected_pools(
    config: &Config,
    pool: Option<String>,
    all: bool,
) -> Result<Vec<String>, WireError> {
    if all == pool.is_some() {
        return Err(WireError::invalid(
            "provide exactly one of pool or all=true",
        ));
    }
    if all {
        return Ok(config.pools.keys().cloned().collect());
    }
    let pool = pool.expect("checked above");
    if !config.pools.contains_key(&pool) {
        return Err(WireError::invalid(format!("unknown pool {pool:?}")));
    }
    Ok(vec![pool])
}

pub(crate) fn find_job<'a>(context: &'a Context, presented: &str) -> Result<&'a Job, WireError> {
    context
        .aliases
        .get(presented)
        .and_then(|job_id| context.jobs.get(job_id))
        .ok_or_else(|| WireError::not_found(format!("job {presented} was not found")))
}

fn insert_job_task_ref(response: &mut Value, job: &Job) {
    if let Some(task_ref) = job.task_ref() {
        response["taskRef"] = Value::String(task_ref.to_string());
    }
}

pub(crate) fn lease_request(job: &Job, unit: String) -> LeaseRequest {
    let scheduling_group = if let Some(orchestration) = &job.row.orchestration {
        LeaseSchedulingGroup::Flow(orchestration.flow_run_id().to_owned())
    } else if let Some(parent) = job.row.parent_uuid {
        LeaseSchedulingGroup::Parent(parent.to_string())
    } else {
        LeaseSchedulingGroup::Standalone
    };
    LeaseRequest {
        job_id: job.job_id.to_string(),
        unit,
        pools: job.row.pools.clone(),
        priority: job.row.priority,
        admission_key: Some(format!("{}:{}", job.stable_key(), job.row.attempt)),
        consumption_estimate: job.row.consumption_estimate,
        scheduling_group,
    }
}

pub(crate) fn state_name(state: JobState) -> &'static str {
    match state {
        JobState::Paused => "paused",
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Completed => "completed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::witness::{build_record, ChainHead};

    #[test]
    fn recovered_smoke_receipt_reads_task_ref_qualified_archived_stderr() {
        let temp = tempfile::tempdir().unwrap();
        let task_uuid = Uuid::parse_str("00000000-0000-4000-8000-000000000007").unwrap();
        let archive = temp
            .path()
            .join("capture/archive")
            .join("00000000-0000-4000-8000-000000000007.t07");
        std::fs::create_dir_all(&archive).unwrap();
        let stderr = archive.join("attempt-0000000001-epoch-00000000000000000007.err");
        std::fs::write(&stderr, b"archived taskRef failure\n").unwrap();

        let orchestration = Orchestration::new(serde_json::json!({
            "flowRunId": "018f5f8e-7b2a-7cc1-8c3a-2dd44ad1f321",
            "taskRef": "crm/t07"
        }))
        .unwrap();
        let record = build_record(
            WitnessBody {
                task_uuid: Some(task_uuid.to_string()),
                transition_timestamp: "2026-08-01T08:00:00.000Z".to_owned(),
                verdict: Verdict::Failed,
                exit_code: 1,
                artifact_content_hash: None,
                store_paths: None,
                drv: None,
                gpu_seconds: None,
                wall_clock: 1.0,
                attempt: 1,
                lease_epoch: 7,
                dedup_key: None,
                payload_hash: None,
                brief_hash: None,
                origin: AdmissionOrigin::direct(EnqueueSource::Orchestrator),
                orchestration: Some(orchestration),
                labor_class: LaborClass::Recovered,
                trace_ref: None,
                pools: vec!["slot".to_owned()],
                executor: None,
                host_id: None,
                charge: None,
                model: None,
                evidence_class: Some(serde_json::json!({"kind": "adapter-smoke"})),
                manifest_hash: None,
                completion: None,
                result_revision: None,
                authorship: None,
                authorship_sessions: None,
            },
            &ChainHead::default(),
        )
        .unwrap();

        let result = job_result_from_witness(&record, &Executor::new(temp.path(), "/bin/true"));
        assert_eq!(
            result.task_ref.as_ref().map(TaskRef::as_str),
            Some("crm/t07")
        );
        assert_eq!(
            result.stderr_excerpt,
            Some(crate::executor::CaptureExcerpt {
                text: "archived taskRef failure\n".to_owned(),
                truncated: false,
            })
        );
        let encoded = result.value();
        assert_eq!(encoded["taskRef"], "crm/t07");
        assert_eq!(encoded["stderr_excerpt"], "archived taskRef failure\n");
    }
}
