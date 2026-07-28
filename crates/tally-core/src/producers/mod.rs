use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use taskchampion::Uuid;
use thiserror::Error;

use crate::adapters::AdapterJobOptions;
use crate::completion::{
    AcceptanceFact, AcceptanceStatus, GateManifestSpec, GateSummary, GateSummaryStatus,
    SemanticCompletion,
};
use crate::config::Priority;
use crate::evidence::parse_evidence_specs;
use crate::taskdb::{
    gh_trigger_dedup_key, gh_trigger_receipt_id, gh_trigger_task_uuid, read_acknowledged_events,
    AdmissionOrigin, EnqueueSource, GhContextSnapshot, GhItemState, GhItemType, GhOrigin,
    GhTriggeringComment, WorkspaceMetadata, GH_CONTEXT_SCHEMA_VERSION, GH_ORIGIN_SCHEMA_VERSION,
    MAX_GH_ORIGIN_FIELD_BYTES,
};
use crate::wire::EnqueuePayload;
use crate::witness::Verdict;

mod config;
mod engine;
mod gh_decision;
mod gh_intake;
mod ingress;
mod validate;

pub use config::*;
pub use engine::*;
pub use gh_decision::*;
pub use gh_intake::*;
pub use ingress::*;
pub use validate::*;

#[derive(Debug, Error)]
pub enum ProducerError {
    #[error("invalid producer configuration: {0}")]
    InvalidConfig(String),
    #[error("invalid producer observation: {0}")]
    InvalidObservation(String),
    #[error("unknown producer {0:?}")]
    UnknownProducer(String),
    #[error("producer {producer:?} has kind {actual:?}, expected {expected:?}")]
    KindMismatch {
        producer: String,
        expected: String,
        actual: String,
    },
    #[error("producer I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("producer JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("durable event error: {0}")]
    DurableEvent(#[from] crate::taskdb::TaskDbError),
    #[error("GitHub COMPLETED mutation failed: {0}")]
    Mutation(String),
    #[error("GitHub trigger acknowledgement failed: {0}")]
    Acknowledgement(String),
    #[error("GitHub intake failed: {0}")]
    GitHub(String),
}

#[cfg(test)]
mod tests;
