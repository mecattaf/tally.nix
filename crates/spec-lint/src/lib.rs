//! The spec layer's enforcement engine.
//!
//! `specs/README.md` is the rule list; this crate is the rule list executed.
//! One parser reads the line grammar of a `specs/<identity>/spec.md` and every
//! rule §7 marks as a single-spec rule reports at most one stderr line per
//! defect: `<file>:<line>: <rule-id>: <message>`.

pub mod claim;
pub mod defect;
pub mod document;
pub mod lexicon;
pub mod lint;
pub mod rules;
pub mod tree;
