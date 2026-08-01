use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use chrono::DateTime;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::adapters::{AdapterConfig, ScrapeStream, TraceFraming};
use crate::executor::{encode_base64, ExecutionIdentity, Executor};
use crate::provenance::TaskRef;
use crate::query::{QUERY_PROTOCOL_VERSION, QUERY_SCHEMA_VERSION};
use crate::query_v2::{FactAuthority, QuerySnapshotMetadata, SourcedValue, TraceAvailability};

const MAX_TRACE_CAPTURE_BYTES: u64 = 16 * 1024 * 1024;
const TRACE_READ_TRUNCATION: &str = "query-read-truncated-at-16777216-bytes";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceLane {
    pub task_uuid: String,
    pub task_ref: Option<TaskRef>,
    pub job_id: Option<String>,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub adapter: String,
    pub session_ref: Option<String>,
    pub running: bool,
    pub remote: bool,
}

impl TraceLane {
    fn identity(&self) -> Option<ExecutionIdentity> {
        let task_uuid = Uuid::parse_str(&self.task_uuid).ok();
        let job_id = self
            .job_id
            .as_deref()
            .and_then(|job| Uuid::parse_str(job).ok())
            .or(task_uuid)?;
        Some(ExecutionIdentity {
            job_id,
            task_uuid,
            task_ref: self.task_ref.clone(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceParseStatus {
    Json,
    Malformed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TraceCapability {
    Available,
    Unavailable,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TraceRetainedRange {
    pub first_record: Option<u64>,
    pub last_record: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TraceRecord {
    pub task_uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
    pub job_id: Option<String>,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub adapter: String,
    pub session_ref: Option<String>,
    pub stream: ScrapeStream,
    pub framing: TraceFraming,
    pub record_index: u64,
    pub cursor: String,
    pub observed_at: Option<String>,
    pub parse_status: TraceParseStatus,
    pub payload: Option<Value>,
    pub raw: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_base64: Option<String>,
    pub authority: FactAuthority,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TraceGeneration {
    pub task_uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ref: Option<TaskRef>,
    pub job_id: Option<String>,
    pub attempt: u32,
    pub lease_epoch: u64,
    pub adapter: String,
    pub stream: Option<ScrapeStream>,
    pub framing: Option<TraceFraming>,
    pub capability: TraceCapability,
    pub complete: bool,
    pub byte_count: Option<u64>,
    pub retained_range: TraceRetainedRange,
    pub truncation: Option<String>,
    pub redaction: SourcedValue<String>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TraceEnvelope {
    pub schema_version: u32,
    pub protocol_version: u32,
    pub items: Vec<TraceRecord>,
    pub next_cursor: Option<String>,
    pub snapshot: QuerySnapshotMetadata,
    pub generations: Vec<TraceGeneration>,
}

#[derive(Debug, Error)]
pub enum TraceError {
    #[error("unknown trace task or job {0:?}")]
    UnknownJob(String),
    #[error("trace task or job {task:?} has no attempt {attempt}")]
    UnknownAttempt { task: String, attempt: u32 },
    #[error("trace capture I/O error at {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

pub fn trace_availability(
    task_or_job: &str,
    lanes: &[TraceLane],
    adapters: &BTreeMap<String, AdapterConfig>,
    executor: &Executor,
) -> TraceAvailability {
    let Some(anchor) = lanes
        .iter()
        .find(|lane| lane.task_uuid == task_or_job || lane.job_id.as_deref() == Some(task_or_job))
        .map(|lane| lane.task_uuid.as_str())
    else {
        return TraceAvailability {
            reason: "no-attempt-trace-metadata".to_owned(),
            ..TraceAvailability::default()
        };
    };
    let mut selected = lanes
        .iter()
        .filter(|lane| lane.task_uuid == anchor)
        .collect::<Vec<_>>();
    selected.sort_by_key(|lane| (lane.attempt, lane.lease_epoch, lane.job_id.clone()));

    let mut available_generations = Vec::new();
    let mut byte_count = 0_u64;
    let mut all_complete = !selected.is_empty();
    let mut query_truncated = false;
    let mut reasons = Vec::new();
    for lane in selected {
        let Some(trace) = adapters
            .get(&lane.adapter)
            .and_then(|adapter| adapter.trace)
        else {
            all_complete = false;
            reasons.push("adapter-does-not-declare-a-provider-trace");
            continue;
        };
        let Some(identity) = lane.identity() else {
            all_complete = false;
            reasons.push("execution-identity-is-not-a-uuid");
            continue;
        };
        let retained =
            match executor.retained_capture_paths(&identity, lane.attempt, lane.lease_epoch) {
                Ok(retained) => retained,
                Err(_) => {
                    all_complete = false;
                    reasons.push("capture-generation-metadata-is-unavailable");
                    continue;
                }
            };
        let Some(retained) = retained else {
            all_complete = false;
            reasons.push(if lane.running && lane.remote {
                "remote-live-trace-unavailable"
            } else if lane.running {
                "local-live-capture-not-yet-available"
            } else {
                "capture-not-retained-for-generation"
            });
            continue;
        };
        let path = match trace.stream {
            ScrapeStream::Stdout => retained.stdout,
            ScrapeStream::Stderr => retained.stderr,
        };
        let metadata = match open_capture(&path) {
            Ok((_, metadata)) => metadata,
            Err(_) => {
                all_complete = false;
                reasons.push("declared-trace-stream-is-not-retained");
                continue;
            }
        };
        byte_count = byte_count.saturating_add(metadata.len());
        if metadata.len() > MAX_TRACE_CAPTURE_BYTES {
            all_complete = false;
            query_truncated = true;
        }
        available_generations.push((lane.attempt, lane.lease_epoch));
        all_complete &= !lane.running;
    }
    reasons.sort_unstable();
    reasons.dedup();

    let available = !available_generations.is_empty();
    let retained_range = available.then(|| {
        let first = available_generations.first().expect("non-empty");
        let last = available_generations.last().expect("non-empty");
        format!(
            "attempt={}/leaseEpoch={}..attempt={}/leaseEpoch={}",
            first.0, first.1, last.0, last.1
        )
    });
    let reason = match (available, all_complete, reasons.is_empty(), query_truncated) {
        (true, _, true, true) => "available-with-explicit-query-truncation".to_owned(),
        (true, true, true, false) => "available-complete".to_owned(),
        (true, _, true, false) => "available-live-snapshot".to_owned(),
        (true, _, false, _) => format!("partially-available:{}", reasons.join(",")),
        (false, _, false, _) => reasons.join(","),
        (false, _, true, _) => "capture-unavailable".to_owned(),
    };
    TraceAvailability {
        available,
        complete: available && all_complete,
        byte_count: available.then_some(byte_count),
        retained_range,
        truncation: query_truncated.then(|| TRACE_READ_TRUNCATION.to_owned()),
        reason,
    }
}

pub fn query_trace(
    task_or_job: &str,
    requested_attempt: Option<u32>,
    lanes: &[TraceLane],
    adapters: &BTreeMap<String, AdapterConfig>,
    executor: &Executor,
    snapshot: QuerySnapshotMetadata,
) -> Result<TraceEnvelope, TraceError> {
    let matching_anchor = lanes
        .iter()
        .find(|lane| lane.task_uuid == task_or_job || lane.job_id.as_deref() == Some(task_or_job))
        .map(|lane| lane.task_uuid.as_str())
        .ok_or_else(|| TraceError::UnknownJob(task_or_job.to_owned()))?;
    let mut selected = lanes
        .iter()
        .filter(|lane| lane.task_uuid == matching_anchor)
        .filter(|lane| requested_attempt.is_none_or(|attempt| lane.attempt == attempt))
        .cloned()
        .collect::<Vec<_>>();
    if let Some(attempt) = requested_attempt {
        if selected.is_empty() {
            return Err(TraceError::UnknownAttempt {
                task: task_or_job.to_owned(),
                attempt,
            });
        }
    }
    selected.sort_by_key(|lane| (lane.attempt, lane.lease_epoch, lane.job_id.clone()));

    let mut items = Vec::new();
    let mut generations = Vec::new();
    for lane in selected {
        let Some(trace) = adapters
            .get(&lane.adapter)
            .and_then(|adapter| adapter.trace)
        else {
            generations.push(unavailable_generation(
                &lane,
                None,
                None,
                TraceCapability::Unsupported,
                "adapter-does-not-declare-a-provider-trace",
            ));
            continue;
        };
        let Some(identity) = lane.identity() else {
            generations.push(unavailable_generation(
                &lane,
                Some(trace.stream),
                Some(trace.framing),
                TraceCapability::Unavailable,
                "execution-identity-is-not-a-uuid",
            ));
            continue;
        };
        let retained =
            match executor.retained_capture_paths(&identity, lane.attempt, lane.lease_epoch) {
                Ok(retained) => retained,
                Err(_) => {
                    generations.push(unavailable_generation(
                        &lane,
                        Some(trace.stream),
                        Some(trace.framing),
                        TraceCapability::Unavailable,
                        "capture-generation-metadata-is-unavailable",
                    ));
                    continue;
                }
            };
        let Some(retained) = retained else {
            let reason = if lane.running && lane.remote {
                "remote-live-trace-unavailable"
            } else if lane.running {
                "local-live-capture-not-yet-available"
            } else {
                "capture-not-retained-for-generation"
            };
            generations.push(unavailable_generation(
                &lane,
                Some(trace.stream),
                Some(trace.framing),
                TraceCapability::Unavailable,
                reason,
            ));
            continue;
        };
        let path = match trace.stream {
            ScrapeStream::Stdout => retained.stdout,
            ScrapeStream::Stderr => retained.stderr,
        };
        let capture = match read_capture(&path) {
            Ok(capture) => capture,
            Err(TraceError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound => {
                generations.push(unavailable_generation(
                    &lane,
                    Some(trace.stream),
                    Some(trace.framing),
                    TraceCapability::Unavailable,
                    "declared-trace-stream-is-not-retained",
                ));
                continue;
            }
            Err(_) => {
                generations.push(unavailable_generation(
                    &lane,
                    Some(trace.stream),
                    Some(trace.framing),
                    TraceCapability::Unavailable,
                    "declared-trace-stream-cannot-be-read",
                ));
                continue;
            }
        };
        let first_index = items.len();
        for (index, raw_bytes) in split_json_lines(&capture.bytes).into_iter().enumerate() {
            let (raw, raw_base64) = match String::from_utf8(raw_bytes.to_vec()) {
                Ok(raw) => (raw, None),
                Err(_) => (
                    String::from_utf8_lossy(raw_bytes).into_owned(),
                    Some(encode_base64(raw_bytes)),
                ),
            };
            let parsed = serde_json::from_slice::<Value>(raw_bytes);
            let (parse_status, payload) = match parsed {
                Ok(payload) => (TraceParseStatus::Json, Some(payload)),
                Err(_) => (TraceParseStatus::Malformed, None),
            };
            let record_index = index as u64;
            let observed_at = payload.as_ref().and_then(observation_time);
            items.push(TraceRecord {
                task_uuid: lane.task_uuid.clone(),
                task_ref: lane.task_ref.clone(),
                job_id: lane.job_id.clone(),
                attempt: lane.attempt,
                lease_epoch: lane.lease_epoch,
                adapter: lane.adapter.clone(),
                session_ref: lane.session_ref.clone(),
                stream: trace.stream,
                framing: trace.framing,
                record_index,
                cursor: trace_cursor(&lane, record_index),
                observed_at,
                parse_status,
                payload,
                raw,
                raw_base64,
                authority: FactAuthority::AdvisoryProviderCapture,
                provenance: "provider-capture".to_owned(),
            });
        }
        let count = items.len() - first_index;
        generations.push(TraceGeneration {
            task_uuid: lane.task_uuid.clone(),
            task_ref: lane.task_ref.clone(),
            job_id: lane.job_id.clone(),
            attempt: lane.attempt,
            lease_epoch: lane.lease_epoch,
            adapter: lane.adapter.clone(),
            stream: Some(trace.stream),
            framing: Some(trace.framing),
            capability: TraceCapability::Available,
            complete: !lane.running && capture.truncation.is_none(),
            byte_count: Some(capture.byte_count),
            retained_range: TraceRetainedRange {
                first_record: (count > 0).then_some(0),
                last_record: (count > 0).then_some(count as u64 - 1),
            },
            truncation: capture.truncation,
            redaction: SourcedValue {
                value: "none".to_owned(),
                authority: FactAuthority::AdvisoryProviderCapture,
                provenance: "provider-capture-retention-policy".to_owned(),
            },
            reason: if capture.byte_count > MAX_TRACE_CAPTURE_BYTES {
                "capture-query-snapshot-truncated"
            } else if lane.running {
                "live-capture-snapshot"
            } else {
                "completed-capture"
            }
            .to_owned(),
        });
    }
    Ok(TraceEnvelope {
        schema_version: QUERY_SCHEMA_VERSION,
        protocol_version: QUERY_PROTOCOL_VERSION,
        items,
        next_cursor: None,
        snapshot,
        generations,
    })
}

fn unavailable_generation(
    lane: &TraceLane,
    stream: Option<ScrapeStream>,
    framing: Option<TraceFraming>,
    capability: TraceCapability,
    reason: &str,
) -> TraceGeneration {
    TraceGeneration {
        task_uuid: lane.task_uuid.clone(),
        task_ref: lane.task_ref.clone(),
        job_id: lane.job_id.clone(),
        attempt: lane.attempt,
        lease_epoch: lane.lease_epoch,
        adapter: lane.adapter.clone(),
        stream,
        framing,
        capability,
        complete: false,
        byte_count: None,
        retained_range: TraceRetainedRange {
            first_record: None,
            last_record: None,
        },
        truncation: None,
        redaction: SourcedValue {
            value: "not-applicable".to_owned(),
            authority: FactAuthority::AdvisoryProviderCapture,
            provenance: "provider-capture-retention-policy".to_owned(),
        },
        reason: reason.to_owned(),
    }
}

fn trace_cursor(lane: &TraceLane, index: u64) -> String {
    format!(
        "trace:{}:{:010}:{:020}:{index:020}",
        lane.task_uuid, lane.attempt, lane.lease_epoch
    )
}

fn split_json_lines(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut records = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    if bytes.last() == Some(&b'\n') {
        records.pop();
    }
    records
}

fn observation_time(payload: &Value) -> Option<String> {
    ["timestamp", "created_at", "createdAt", "observedAt"]
        .into_iter()
        .filter_map(|key| payload.get(key).and_then(Value::as_str))
        .find(|value| DateTime::parse_from_rfc3339(value).is_ok())
        .map(ToOwned::to_owned)
}

struct CaptureRead {
    bytes: Vec<u8>,
    byte_count: u64,
    truncation: Option<String>,
}

fn read_capture(path: &Path) -> Result<CaptureRead, TraceError> {
    let (file, metadata) = open_capture(path)?;
    let byte_count = metadata.len();
    let mut bytes = Vec::with_capacity(byte_count.min(MAX_TRACE_CAPTURE_BYTES) as usize);
    file.take(MAX_TRACE_CAPTURE_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|source| TraceError::Io {
            path: path.display().to_string(),
            source,
        })?;
    let truncation =
        (byte_count > MAX_TRACE_CAPTURE_BYTES).then(|| TRACE_READ_TRUNCATION.to_owned());
    if truncation.is_some() && bytes.last() != Some(&b'\n') {
        let complete = bytes
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |position| position + 1);
        bytes.truncate(complete);
    }
    Ok(CaptureRead {
        bytes,
        byte_count,
        truncation,
    })
}

fn open_capture(path: &Path) -> Result<(std::fs::File, std::fs::Metadata), TraceError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|source| TraceError::Io {
            path: path.display().to_string(),
            source,
        })?;
    let metadata = file.metadata().map_err(|source| TraceError::Io {
        path: path.display().to_string(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(TraceError::Io {
            path: path.display().to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "trace capture is not a regular file",
            ),
        });
    }
    Ok((file, metadata))
}

#[cfg(test)]
mod tests {
    use std::io::{Seek, SeekFrom, Write};

    use super::*;
    use crate::adapters::{AdapterTrace, TraceFraming};
    use crate::history::RetentionMetadata;
    use crate::query_v2::{QueryChainHead, QuerySnapshotMetadata};
    use proptest::prelude::*;

    fn snapshot() -> QuerySnapshotMetadata {
        QuerySnapshotMetadata {
            created_at: chrono::Utc::now().to_rfc3339(),
            cursor: None,
            history: RetentionMetadata {
                complete: true,
                policy: crate::history::LIFECYCLE_RETENTION_POLICY.to_owned(),
                earliest_cursor: None,
                latest_cursor: None,
                truncation_boundary: None,
                reason: None,
            },
            witness_head: QueryChainHead {
                seq: 0,
                hash: "genesis".to_owned(),
            },
        }
    }

    #[test]
    fn json_unknown_and_malformed_lines_remain_ordered_and_advisory() {
        let temp = tempfile::tempdir().unwrap();
        let executor = Executor::new(temp.path(), "/bin/true");
        let id = Uuid::new_v4();
        let identity = ExecutionIdentity {
            job_id: id,
            task_uuid: Some(id),
            task_ref: None,
        };
        let paths = executor.paths(&identity);
        std::fs::create_dir_all(paths.stdout.parent().unwrap()).unwrap();
        std::fs::create_dir_all(paths.capture_generation.parent().unwrap()).unwrap();
        std::fs::write(
            &paths.stdout,
            b"{\"type\":\"message\",\"text\":\"hello\"}\n{\"type\":\"future-event\",\"x\":1}\n{malformed\n\xffbinary\n",
        )
        .unwrap();
        std::fs::write(&paths.stderr, b"").unwrap();
        std::fs::write(
            &paths.capture_generation,
            b"{\"attempt\":1,\"leaseEpoch\":7}",
        )
        .unwrap();
        let adapters = BTreeMap::from([(
            "codex".to_owned(),
            AdapterConfig {
                trace: Some(AdapterTrace {
                    stream: ScrapeStream::Stdout,
                    framing: TraceFraming::JsonLines,
                }),
                ..AdapterConfig::default()
            },
        )]);
        let lane = TraceLane {
            task_uuid: id.to_string(),
            task_ref: None,
            job_id: Some(id.to_string()),
            attempt: 1,
            lease_epoch: 7,
            adapter: "codex".to_owned(),
            session_ref: None,
            running: false,
            remote: false,
        };
        let trace = query_trace(
            &id.to_string(),
            None,
            &[lane],
            &adapters,
            &executor,
            snapshot(),
        )
        .unwrap();
        assert_eq!(trace.items.len(), 4);
        assert_eq!(
            trace.items[1].payload.as_ref().unwrap()["type"],
            "future-event"
        );
        assert_eq!(trace.items[2].parse_status, TraceParseStatus::Malformed);
        assert_eq!(trace.items[2].raw, "{malformed");
        assert_eq!(trace.items[3].parse_status, TraceParseStatus::Malformed);
        assert_eq!(
            trace.items[3].raw_base64.as_deref(),
            Some(encode_base64(b"\xffbinary").as_str())
        );
        assert!(trace
            .items
            .iter()
            .all(|item| item.authority == FactAuthority::AdvisoryProviderCapture));
    }

    #[test]
    fn acceptance_24_3_claude_and_codex_jsonl_are_lossless_ordered_and_advisory() {
        for (adapter, fixture) in [
            (
                "claude-code",
                include_str!("../../../test/fixtures/traces/claude-code.jsonl"),
            ),
            (
                "codex",
                include_str!("../../../test/fixtures/traces/codex.jsonl"),
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let executor = Executor::new(temp.path(), "/bin/true");
            let id = Uuid::new_v4();
            let identity = ExecutionIdentity {
                job_id: id,
                task_uuid: Some(id),
                task_ref: None,
            };
            let paths = executor.paths(&identity);
            std::fs::create_dir_all(paths.stdout.parent().unwrap()).unwrap();
            std::fs::create_dir_all(paths.capture_generation.parent().unwrap()).unwrap();
            std::fs::write(&paths.stdout, fixture).unwrap();
            std::fs::write(&paths.stderr, b"").unwrap();
            std::fs::write(
                &paths.capture_generation,
                b"{\"attempt\":1,\"leaseEpoch\":7}",
            )
            .unwrap();
            let adapters = BTreeMap::from([(
                adapter.to_owned(),
                AdapterConfig {
                    trace: Some(AdapterTrace {
                        stream: ScrapeStream::Stdout,
                        framing: TraceFraming::JsonLines,
                    }),
                    ..AdapterConfig::default()
                },
            )]);
            let lane = TraceLane {
                task_uuid: id.to_string(),
                task_ref: None,
                job_id: Some(id.to_string()),
                attempt: 1,
                lease_epoch: 7,
                adapter: adapter.to_owned(),
                session_ref: Some(format!("{adapter}-session")),
                running: false,
                remote: false,
            };

            let trace = query_trace(
                &id.to_string(),
                None,
                &[lane],
                &adapters,
                &executor,
                snapshot(),
            )
            .unwrap();
            let expected = fixture.lines().collect::<Vec<_>>();
            assert_eq!(
                trace
                    .items
                    .iter()
                    .map(|record| record.raw.as_str())
                    .collect::<Vec<_>>(),
                expected,
                "{adapter}"
            );
            assert_eq!(
                trace.items.last().unwrap().parse_status,
                TraceParseStatus::Malformed
            );
            assert_eq!(
                trace.items[expected.len() - 2].parse_status,
                TraceParseStatus::Json
            );
            assert_eq!(
                trace.items[expected.len() - 2].payload.as_ref().unwrap()["extension"]["preserve"],
                "verbatim"
            );
            let payloads = trace
                .items
                .iter()
                .filter_map(|record| record.payload.as_ref())
                .collect::<Vec<_>>();
            if adapter == "claude-code" {
                assert!(payloads.iter().any(|payload| {
                    payload["message"]["content"]
                        .as_array()
                        .is_some_and(|content| content.iter().any(|item| item["type"] == "text"))
                }));
                assert!(payloads.iter().any(|payload| {
                    payload["message"]["content"]
                        .as_array()
                        .is_some_and(|content| {
                            content.iter().any(|item| item["type"] == "tool_use")
                        })
                }));
                assert!(payloads.iter().any(|payload| {
                    payload["message"]["content"]
                        .as_array()
                        .is_some_and(|content| {
                            content.iter().any(|item| item["type"] == "tool_result")
                        })
                }));
            } else {
                assert!(payloads
                    .iter()
                    .any(|payload| payload["item"]["type"] == "agent_message"));
                assert!(payloads
                    .iter()
                    .any(|payload| payload["item"]["type"] == "command_execution"
                        && payload["item"]["status"] == "completed"));
            }
            assert!(payloads
                .iter()
                .any(|payload| payload.get("usage").is_some()));
            assert!(trace.items.iter().all(|record| {
                record.authority == FactAuthority::AdvisoryProviderCapture
                    && record.provenance == "provider-capture"
            }));
        }
    }

    #[test]
    fn acceptance_24_4_running_local_and_remote_traces_are_never_silently_empty() {
        let temp = tempfile::tempdir().unwrap();
        let executor = Executor::new(temp.path(), "/bin/true");
        let task = Uuid::new_v4().to_string();
        let adapters = BTreeMap::from([(
            "codex".to_owned(),
            AdapterConfig {
                trace: Some(AdapterTrace {
                    stream: ScrapeStream::Stdout,
                    framing: TraceFraming::JsonLines,
                }),
                ..AdapterConfig::default()
            },
        )]);
        for (remote, expected_reason) in [
            (false, "local-live-capture-not-yet-available"),
            (true, "remote-live-trace-unavailable"),
        ] {
            let lane = TraceLane {
                task_uuid: task.clone(),
                task_ref: None,
                job_id: Some(task.clone()),
                attempt: 1,
                lease_epoch: 7,
                adapter: "codex".to_owned(),
                session_ref: None,
                running: true,
                remote,
            };
            let trace =
                query_trace(&task, None, &[lane], &adapters, &executor, snapshot()).unwrap();
            assert!(trace.items.is_empty());
            assert_eq!(trace.generations.len(), 1);
            assert_eq!(
                trace.generations[0].capability,
                TraceCapability::Unavailable
            );
            assert_eq!(trace.generations[0].reason, expected_reason);
        }
    }

    proptest! {
        #[test]
        fn oversized_capture_discards_every_arbitrary_incomplete_suffix(
            incomplete_suffix in prop::collection::vec(
                prop_oneof![0_u8..=9, 11_u8..=u8::MAX],
                0..4097,
            ),
            overflow_bytes in 1_u64..=4096,
        ) {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("capture.jsonl");
            let mut file = std::fs::File::create(&path).unwrap();
            file.set_len(MAX_TRACE_CAPTURE_BYTES + overflow_bytes).unwrap();
            let last_newline = MAX_TRACE_CAPTURE_BYTES
                - incomplete_suffix.len() as u64
                - 1;
            file.seek(SeekFrom::Start(last_newline)).unwrap();
            file.write_all(b"\n").unwrap();
            file.write_all(&incomplete_suffix).unwrap();
            file.sync_all().unwrap();
            drop(file);

            let capture = read_capture(&path).unwrap();
            prop_assert_eq!(
                capture.byte_count,
                MAX_TRACE_CAPTURE_BYTES + overflow_bytes,
            );
            prop_assert_eq!(
                capture.truncation.as_deref(),
                Some(TRACE_READ_TRUNCATION),
            );
            prop_assert_eq!(capture.bytes.len() as u64, last_newline + 1);
            prop_assert_eq!(capture.bytes.last(), Some(&b'\n'));
        }
    }
}
