use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Priority;
use crate::producers::{
    read_producer_runtime, ProducerConfig, ProducerEnqueue, ProducerRuntimeRecord,
};
use crate::query::{QUERY_PROTOCOL_VERSION, QUERY_SCHEMA_VERSION};
use crate::query_v2::FactAuthority;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerUnitIdentity {
    pub service: String,
    pub timer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerSchedule {
    pub calendar_expression: Option<String>,
    pub poll_cadence_sec: Option<u64>,
    pub next_trigger: Option<String>,
    pub next_trigger_unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerRuntimeProjection {
    pub state: String,
    pub state_reason: String,
    pub last_trigger: Option<String>,
    pub last_emission: Option<String>,
    pub last_outcome: Option<Value>,
    pub last_error: Option<String>,
    pub authority: FactAuthority,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerEnqueueSummary {
    pub action: String,
    #[serde(
        rename = "pool",
        serialize_with = "crate::poolset::serialize",
        deserialize_with = "crate::poolset::deserialize"
    )]
    pub pools: Vec<String>,
    pub executor: Option<String>,
    pub priority: Priority,
    pub adapter: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerProjection {
    pub name: String,
    pub kind: String,
    pub configured: bool,
    pub enabled: bool,
    pub unit: ProducerUnitIdentity,
    pub schedule: ProducerSchedule,
    pub enqueue: Vec<ProducerEnqueueSummary>,
    pub runtime: ProducerRuntimeProjection,
    pub configuration_authority: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducersView {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub items: Vec<ProducerProjection>,
    pub next_cursor: Option<String>,
    pub snapshot: ProducerSnapshotMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProducerSnapshotMetadata {
    pub created_at: String,
    pub cursor: Option<String>,
    pub configuration_authority: String,
    pub runtime_authority: FactAuthority,
}

pub fn query_producers(
    registry: &BTreeMap<String, ProducerConfig>,
    state_dir: &Path,
    name_filter: Option<&str>,
    kind_filter: Option<&str>,
) -> ProducersView {
    let mut items = registry
        .iter()
        .filter(|(name, producer)| {
            name_filter.is_none_or(|filter| filter == name.as_str())
                && kind_filter.is_none_or(|filter| filter == producer.kind())
        })
        .map(|(name, producer)| project_producer(name, producer, state_dir))
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.name.cmp(&right.name));
    ProducersView {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        items,
        next_cursor: None,
        snapshot: ProducerSnapshotMetadata {
            created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
            cursor: None,
            configuration_authority: "nix-effective-declarative-registry".to_owned(),
            runtime_authority: FactAuthority::TallyLifecycleObservation,
        },
    }
}

fn project_producer(name: &str, producer: &ProducerConfig, state_dir: &Path) -> ProducerProjection {
    let runtime = read_producer_runtime(state_dir, name);
    let runtime_record = runtime.as_ref().ok().and_then(Clone::clone);
    let (calendar_expression, poll_cadence_sec) = schedule(producer);
    let (next_trigger, next_trigger_unavailable_reason) =
        next_trigger(producer, runtime_record.as_ref(), poll_cadence_sec);
    let enabled = true;
    let runtime_projection = match runtime {
        Ok(Some(record)) if record.last_error.is_some() => ProducerRuntimeProjection {
            state: "failed".to_owned(),
            state_reason: "last-dispatch-failed".to_owned(),
            last_trigger: Some(record.last_trigger),
            last_emission: record.last_emission,
            last_outcome: record.last_outcome,
            last_error: record.last_error,
            authority: FactAuthority::TallyLifecycleObservation,
        },
        Ok(Some(record)) => ProducerRuntimeProjection {
            state: "unknown".to_owned(),
            state_reason: "last-dispatch-succeeded-but-unit-active-state-is-not-observed"
                .to_owned(),
            last_trigger: Some(record.last_trigger),
            last_emission: record.last_emission,
            last_outcome: record.last_outcome,
            last_error: None,
            authority: FactAuthority::TallyLifecycleObservation,
        },
        Ok(None) => ProducerRuntimeProjection {
            state: "unknown".to_owned(),
            state_reason: if enabled {
                "no-runtime-observation-recorded"
            } else {
                "producer-disabled-by-effective-configuration"
            }
            .to_owned(),
            last_trigger: None,
            last_emission: None,
            last_outcome: None,
            last_error: None,
            authority: FactAuthority::TallyLifecycleObservation,
        },
        Err(error) => ProducerRuntimeProjection {
            state: "failed".to_owned(),
            state_reason: "runtime-state-invalid".to_owned(),
            last_trigger: None,
            last_emission: None,
            last_outcome: None,
            last_error: Some(error.to_string()),
            authority: FactAuthority::TallyLifecycleObservation,
        },
    };
    ProducerProjection {
        name: name.to_owned(),
        kind: producer.kind().to_owned(),
        configured: true,
        enabled,
        unit: ProducerUnitIdentity {
            service: format!("tally-producer-{name}.service"),
            timer: matches!(
                producer,
                ProducerConfig::Calendar(_) | ProducerConfig::EventsDir(_)
            )
            .then(|| format!("tally-producer-{name}.timer")),
        },
        schedule: ProducerSchedule {
            calendar_expression,
            poll_cadence_sec,
            next_trigger,
            next_trigger_unavailable_reason,
        },
        enqueue: enqueue_summaries(producer),
        runtime: runtime_projection,
        configuration_authority: "nix-declarative-configuration".to_owned(),
    }
}

fn schedule(producer: &ProducerConfig) -> (Option<String>, Option<u64>) {
    match producer {
        ProducerConfig::Calendar(config) => (Some(config.on_calendar.clone()), None),
        ProducerConfig::EventsDir(config) => (None, Some(config.poll_interval_sec)),
    }
}

fn next_trigger(
    producer: &ProducerConfig,
    runtime: Option<&ProducerRuntimeRecord>,
    cadence: Option<u64>,
) -> (Option<String>, Option<String>) {
    if matches!(producer, ProducerConfig::Calendar(_)) {
        return (
            None,
            Some("systemd-calendar-next-trigger-is-not-available-to-the-daemon".to_owned()),
        );
    }
    let Some(cadence) = cadence else {
        return (
            None,
            Some("producer-is-event-driven-without-a-fixed-next-trigger".to_owned()),
        );
    };
    let Some(last) = runtime.and_then(|record| {
        DateTime::parse_from_rfc3339(&record.last_trigger)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc))
    }) else {
        return (
            None,
            Some("no-last-trigger-is-available-for-cadence-projection".to_owned()),
        );
    };
    let Ok(cadence) = i64::try_from(cadence) else {
        return (
            None,
            Some("poll-cadence-exceeds-supported-timestamp-range".to_owned()),
        );
    };
    let Some(next) = last.checked_add_signed(Duration::seconds(cadence)) else {
        return (
            None,
            Some("next-trigger-exceeds-supported-timestamp-range".to_owned()),
        );
    };
    (
        Some(next.to_rfc3339_opts(SecondsFormat::Millis, true)),
        None,
    )
}

fn enqueue_summaries(producer: &ProducerConfig) -> Vec<ProducerEnqueueSummary> {
    match producer {
        ProducerConfig::Calendar(config) => {
            vec![enqueue_summary("calendar", "calendar", &config.enqueue)]
        }
        ProducerConfig::EventsDir(_) => Vec::new(),
    }
}

fn enqueue_summary(
    action: &str,
    source: &str,
    enqueue: &ProducerEnqueue,
) -> ProducerEnqueueSummary {
    ProducerEnqueueSummary {
        action: action.to_owned(),
        pools: enqueue.pools.clone(),
        executor: enqueue.executor.clone(),
        priority: enqueue.priority,
        adapter: enqueue.adapter.clone(),
        source: source.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::producers::{record_producer_runtime, CalendarProducer, ProducerEnqueue};

    #[test]
    fn calendar_inventory_exposes_identity_schedule_and_runtime_reason_without_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let registry = BTreeMap::from([(
            "nightly".to_owned(),
            ProducerConfig::Calendar(Box::new(CalendarProducer {
                credentials: BTreeMap::from([(
                    "token".to_owned(),
                    "/run/secrets/never-project-this".into(),
                )]),
                on_calendar: "daily".to_owned(),
                enqueue: ProducerEnqueue {
                    pools: vec!["slot".to_owned()],
                    ..serde_json::from_value(serde_json::json!({
                        "argv": ["true"],
                        "pool": "slot"
                    }))
                    .unwrap()
                },
            })),
        )]);
        record_producer_runtime(
            temp.path(),
            "nightly",
            DateTime::parse_from_rfc3339("2026-07-24T03:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            Some(Value::String(
                "/state/events/nightly-calendar.producer.json".to_owned(),
            )),
            None,
        )
        .unwrap();
        record_producer_runtime(
            temp.path(),
            "nightly",
            DateTime::parse_from_rfc3339("2026-07-24T04:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            Some(Value::String("no-emission".to_owned())),
            None,
        )
        .unwrap();
        let view = query_producers(&registry, temp.path(), None, None);
        let item = &view.items[0];
        assert_eq!(
            item.unit.timer.as_deref(),
            Some("tally-producer-nightly.timer")
        );
        assert_eq!(item.schedule.calendar_expression.as_deref(), Some("daily"));
        assert!(item
            .schedule
            .next_trigger_unavailable_reason
            .as_deref()
            .is_some());
        assert_eq!(
            item.runtime.last_trigger.as_deref(),
            Some("2026-07-24T04:00:00+00:00")
        );
        assert_eq!(
            item.runtime.last_emission.as_deref(),
            Some("2026-07-24T03:00:00+00:00")
        );
        assert_eq!(
            item.runtime.last_outcome,
            Some(Value::String("no-emission".to_owned()))
        );
        assert!(view.next_cursor.is_none());
        assert_eq!(
            view.snapshot.configuration_authority,
            "nix-effective-declarative-registry"
        );
        let encoded = serde_json::to_string(&view).unwrap();
        assert!(!encoded.contains("never-project-this"));
    }
}
