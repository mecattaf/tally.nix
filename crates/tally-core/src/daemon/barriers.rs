use super::*;

pub(crate) enum WaitRegistration {
    Ready(Value),
    Pending(oneshot::Receiver<Value>),
}

#[derive(Debug, Default)]
pub(crate) struct BarrierEntry {
    pub(crate) pending: BTreeSet<String>,
    pub(crate) results: BTreeMap<String, Value>,
    pub(crate) waiters: Vec<oneshot::Sender<Value>>,
}

#[derive(Debug, Default)]
pub struct BarrierTracker {
    pub(crate) namespace: u64,
    pub(crate) next: u64,
    pub(crate) barriers: HashMap<String, BarrierEntry>,
    pub(crate) job_waiters: HashMap<String, Vec<oneshot::Sender<Value>>>,
}

impl BarrierTracker {
    pub fn with_namespace(namespace: u64) -> Self {
        Self {
            namespace,
            ..Self::default()
        }
    }

    pub fn register_job(&mut self, stable_job_key: &str, attempt: u32) -> String {
        self.prune_closed_waiters();
        format!("barrier:{stable_job_key}:{attempt}")
    }

    pub fn snapshot(&mut self, jobs: impl IntoIterator<Item = String>) -> String {
        self.prune_closed_waiters();
        self.next = self.next.saturating_add(1);
        let barrier = format!("barrier:drain:{}:{}", self.namespace, self.next);
        let mut entry = BarrierEntry::default();
        for job in jobs {
            entry.pending.insert(job);
        }
        self.barriers.insert(barrier.clone(), entry);
        self.prune_unclaimed_barriers();
        barrier
    }

    pub(crate) fn complete_job(&mut self, stable_job_key: &str, value: Value) {
        self.prune_closed_waiters();
        if let Some(waiters) = self.job_waiters.remove(stable_job_key) {
            for waiter in waiters {
                let _ = waiter.send(value.clone());
            }
        }

        let mut completed = Vec::new();
        for (barrier, entry) in &mut self.barriers {
            if entry.pending.remove(stable_job_key) {
                entry
                    .results
                    .insert(stable_job_key.to_owned(), value.clone());
            }
            if entry.pending.is_empty() && !entry.waiters.is_empty() {
                completed.push(barrier.clone());
            }
        }
        for barrier in completed {
            if let Some(mut entry) = self.barriers.remove(&barrier) {
                let result = barrier_value(&barrier, &entry.results);
                if entry.waiters.is_empty() {
                    self.barriers.insert(barrier, entry);
                } else {
                    for waiter in std::mem::take(&mut entry.waiters) {
                        let _ = waiter.send(result.clone());
                    }
                }
            }
        }
        self.prune_unclaimed_barriers();
    }

    pub(crate) fn wait_job(&mut self, stable_job_key: &str) -> WaitRegistration {
        self.prune_closed_waiters();
        let (sender, receiver) = oneshot::channel();
        self.job_waiters
            .entry(stable_job_key.to_owned())
            .or_default()
            .push(sender);
        WaitRegistration::Pending(receiver)
    }

    fn prune_closed_waiters(&mut self) {
        self.job_waiters.retain(|_, waiters| {
            waiters.retain(|waiter| !waiter.is_closed());
            !waiters.is_empty()
        });
        for entry in self.barriers.values_mut() {
            entry.waiters.retain(|waiter| !waiter.is_closed());
        }
    }

    fn prune_unclaimed_barriers(&mut self) {
        let mut unclaimed = self
            .barriers
            .iter()
            .filter(|(_, entry)| entry.waiters.is_empty())
            .map(|(barrier, _)| {
                let sequence = barrier
                    .rsplit(':')
                    .next()
                    .and_then(|sequence| sequence.parse::<u64>().ok())
                    .unwrap_or(0);
                (sequence, barrier.clone())
            })
            .collect::<Vec<_>>();
        unclaimed.sort_by_key(|(sequence, _)| *sequence);
        let remove_count = unclaimed
            .len()
            .saturating_sub(UNCLAIMED_DRAIN_BARRIER_LIMIT);
        for (_, barrier) in unclaimed.into_iter().take(remove_count) {
            self.barriers.remove(&barrier);
        }
    }

    pub(crate) fn wait_barrier(&mut self, barrier: &str) -> Result<WaitRegistration, WireError> {
        self.prune_closed_waiters();
        if self
            .barriers
            .get(barrier)
            .is_some_and(|entry| entry.pending.is_empty())
        {
            let entry = self
                .barriers
                .remove(barrier)
                .expect("the completed barrier was just observed");
            return Ok(WaitRegistration::Ready(barrier_value(
                barrier,
                &entry.results,
            )));
        }
        let entry = self
            .barriers
            .get_mut(barrier)
            .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
        let (sender, receiver) = oneshot::channel();
        entry.waiters.push(sender);
        Ok(WaitRegistration::Pending(receiver))
    }

    #[cfg(test)]
    pub(crate) fn retained_entry_count(&self) -> usize {
        self.barriers.len() + self.job_waiters.values().map(Vec::len).sum::<usize>()
    }
}

fn barrier_value(barrier: &str, results: &BTreeMap<String, Value>) -> Value {
    json!({
        "barrier": barrier,
        "complete": true,
        "results": results.values().cloned().collect::<Vec<_>>(),
    })
}

pub(crate) async fn await_registration(registration: WaitRegistration) -> Result<Value, WireError> {
    match registration {
        WaitRegistration::Ready(value) => Ok(value),
        WaitRegistration::Pending(receiver) => receiver
            .await
            .map_err(|_| internal_wire("daemon stopped while waiting")),
    }
}

pub(crate) fn parse_job_barrier(barrier: &str) -> Result<(&str, u32), WireError> {
    let body = barrier
        .strip_prefix("barrier:")
        .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
    let (stable, attempt) = body
        .rsplit_once(':')
        .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
    if stable.is_empty() || stable.starts_with("drain:") {
        return Err(WireError::not_found(format!("unknown barrier {barrier}")));
    }
    let attempt = attempt
        .parse::<u32>()
        .ok()
        .filter(|attempt| *attempt > 0)
        .ok_or_else(|| WireError::not_found(format!("unknown barrier {barrier}")))?;
    Ok((stable, attempt))
}

pub(crate) fn single_job_barrier_value(barrier: &str, stable: &str, result: Value) -> Value {
    barrier_value(barrier, &BTreeMap::from([(stable.to_owned(), result)]))
}
