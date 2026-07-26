//! Core types and validation shared by the tally daemon and CLI.

pub mod adapters;
pub mod brief;
pub mod completion;
pub mod config;
pub mod daemon;
pub mod evidence;
pub mod exec_attestation;
pub mod executor;
pub mod git_ai;
pub mod history;
pub mod journal;
pub mod lease;
pub mod nix_store;
pub mod pagination;
pub mod poolset;
pub mod producer_query;
pub mod producers;
pub mod provenance;
pub mod query;
pub mod query_v2;
pub mod recovery;
pub mod retention;
pub mod taskdb;
pub mod trace;
pub mod view;
pub mod watch;
pub mod wire;
pub mod witness;

pub use config::{Config, ConfigError, Enforce, Priority};
