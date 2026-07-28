//! Deterministic JavaScript orchestration for tally flow runs.
#![allow(
    clippy::result_large_err,
    reason = "FlowError is the public structured dialect report, including typed details and stack"
)]

use std::time::Duration;

mod catalog;
mod client;
mod dialect;
mod engine;
mod error;
mod executor;
mod model;

pub use catalog::{
    catalog_schema, load_catalog, resolve_members, Catalog, CatalogMember, CatalogSelection,
    SelectorOptions,
};
pub use client::{FlowClient, FlowFuture};
pub use dialect::{check_script, validate_flow_pool_predicates, CheckOptions, CheckedFlow, Meta};
pub use engine::{run_script, LifecycleSink, RunOptions, VecLifecycleSink};
pub use error::{FlowError, SourceLocation};
pub use model::{
    flow_canonical_payload_fields, node_spec_fields, Admission, ClientError, Derivation,
    DerivationOutput, Disposition, FlowEnqueueFieldDisposition, FlowEnqueueFieldParity,
    FlowSubmission, NodeCanonicalProjection, NodeFailure, NodeResult, NodeSpec,
    NodeSpecFieldContract, NodeSpecSurface, NodeWireProjection, Orchestration, RunInspection,
    RunReport, SelectionProvenance, Verdict, FLOW_ENQUEUE_FIELD_PARITY, NODE_SPEC_FIELD_CONTRACT,
};

/// The one prompt-delivery argument used by every agent adapter sugar.
pub const BRIEF_SENTINEL: &str = "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set.";

/// Default per-flow materialized-node ceiling.
pub const DEFAULT_MAX_NODES: u32 = 1_000;

/// Default per-call-site iteration ceiling.
pub const DEFAULT_ITERATION_CAP: u32 = 64;

/// Uncatchable Boa loop backstop, distinct from the host call-site iteration cap.
pub const ENGINE_LOOP_LIMIT: u64 = 1_000_000;

/// Uncatchable Boa recursion backstop.
pub const ENGINE_RECURSION_LIMIT: usize = 512;

/// Total number of synchronous promise/generic jobs one flow evaluation may run.
pub const ENGINE_MICROTASK_LIMIT: u64 = 100_000;

/// Total elapsed budget for one flow evaluation, including awaited host work.
pub const ENGINE_WALL_CLOCK_LIMIT: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg(test)]
mod tests {
    use super::BRIEF_SENTINEL;

    #[test]
    fn prompt_delivery_sentinel_is_frozen_verbatim() {
        assert_eq!(
            BRIEF_SENTINEL,
            "Read the file whose path is in the TALLY_BRIEF environment variable and execute the mission it contains. That brief is your complete instruction set."
        );
        assert!(!BRIEF_SENTINEL.contains('$'));
        assert!(!BRIEF_SENTINEL.contains('/'));
    }
}
