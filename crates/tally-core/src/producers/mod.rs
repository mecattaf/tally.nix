//! Canonical producer ingress and runtime observations.
//!
//! Claimed/archived ingress bytes and last-runtime records are original input
//! or observation surfaces. Producer query values are derived from these files
//! and declared configuration.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::adapters::AdapterJobOptions;
use crate::completion::GateManifestSpec;
use crate::config::Priority;
use crate::evidence::parse_evidence_specs;
use crate::taskdb::{read_acknowledged_events, AdmissionOrigin, EnqueueSource, WorkspaceMetadata};
use crate::wire::EnqueuePayload;

mod config;
mod engine;
mod ingress;
mod validate;

pub use config::*;
pub use engine::*;
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
}

#[cfg(test)]
mod tests;
