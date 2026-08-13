use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmitOutcome {
    Emitted(PathBuf),
    Duplicate,
}

pub struct ProducerEngine<'a> {
    registry: &'a BTreeMap<String, ProducerConfig>,
    events_dir: PathBuf,
    brief_root: PathBuf,
}

impl<'a> ProducerEngine<'a> {
    pub fn new(
        registry: &'a BTreeMap<String, ProducerConfig>,
        events_dir: impl Into<PathBuf>,
        brief_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            registry,
            events_dir: events_dir.into(),
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
            .payload(EnqueueSource::Calendar, Some(producer), now)?;
        let name = format!("{producer}-calendar-{}{}", Uuid::new_v4(), INGRESS_SUFFIX);
        self.emit_named(&name, &payload)
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

pub(super) fn stable_key(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}
