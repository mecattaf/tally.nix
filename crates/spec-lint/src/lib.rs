//! The spec layer's enforcement engine.
//!
//! `specs/README.md` is the rule list; this crate is the rule list executed.
//! One parser reads the line grammar of a `specs/<identity>/spec.md` and three
//! modes read what it produces:
//!
//! - **check** (the default) — every rule §7 marks as a single-spec rule, then
//!   the cross-artifact pass that joins the spec to its governing worklist and
//!   to `trace.json`. Each defect reports one stderr line
//!   `<file>:<line>: <rule-id>: <message>`.
//! - **census** — every claim's oracle binding, enumerated (§6).
//! - **coverage** — the claim ↔ task ↔ acceptance-id ↔ evidence join, rendered
//!   as the markdown table an operator hands to release.

pub mod artifacts;
pub mod census;
pub mod claim;
pub mod coverage;
pub mod defect;
pub mod document;
pub mod index;
pub mod lexicon;
pub mod lint;
pub mod resolution;
pub mod rules;
pub mod schema;
pub mod tree;
