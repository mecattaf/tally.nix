//! Core types and validation shared by the tally daemon and CLI.

pub mod adapters;
pub mod completion;
pub mod config;
pub mod daemon;
pub mod evidence;
pub mod executor;
pub mod journal;
pub mod lease;
pub mod poolset;
pub mod producers;
pub mod query;
pub mod recovery;
pub mod taskdb;
pub mod wire;
pub mod witness;

pub use config::{Config, ConfigError, Enforce, Priority};
