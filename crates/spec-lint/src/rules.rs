//! The rule index of `specs/README.md` §7, transcribed as bytes a test can
//! compare against that table. Drift between the table and this catalog fails
//! `cargo-tests` — the README names this crate as its standing consumer.

use std::fmt;

/// A rule id from the `specs/README.md` §7 index.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RuleId {
    L1,
    L2,
    L3,
    L4,
    L5,
    L6,
    L7,
    L8,
    L9,
    L10,
    L11,
    L12,
    L13,
    L14,
    L15,
    L16,
    L17,
    L18,
}

impl RuleId {
    /// The id as it is printed on a defect line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::L1 => "L1",
            Self::L2 => "L2",
            Self::L3 => "L3",
            Self::L4 => "L4",
            Self::L5 => "L5",
            Self::L6 => "L6",
            Self::L7 => "L7",
            Self::L8 => "L8",
            Self::L9 => "L9",
            Self::L10 => "L10",
            Self::L11 => "L11",
            Self::L12 => "L12",
            Self::L13 => "L13",
            Self::L14 => "L14",
            Self::L15 => "L15",
            Self::L16 => "L16",
            Self::L17 => "L17",
            Self::L18 => "L18",
        }
    }

    /// Parse a printed id back, so a corpus map keyed by rule id resolves.
    pub fn parse(text: &str) -> Option<Self> {
        CATALOG
            .iter()
            .map(|rule| rule.id)
            .find(|id| id.as_str() == text)
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which half of the linter evaluates a rule. The cross-artifact half —
/// worklist pointers, trace rows, append-only history — belongs to the
/// resolution pass; it is catalogued here so the README parity test has a
/// complete rule set to compare against.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stage {
    /// Evaluated by this crate's `check` pass over one `spec.md`.
    Core,
    /// Evaluated by the cross-artifact resolution pass, which runs inside the
    /// check mode over the governing worklist and `trace.json`. L13, L14, and
    /// L18 run there today; L17 compares the trace against its parent revision
    /// and waits on sitting mode, the only place a parent revision exists.
    Resolution,
}

/// One row of the `specs/README.md` §7 rule index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rule {
    pub id: RuleId,
    /// The severity cell of the README row, byte for byte.
    pub severity: &'static str,
    pub stage: Stage,
}

/// Every rule the README enumerates, in README order.
pub const CATALOG: [Rule; 18] = [
    Rule {
        id: RuleId::L1,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L2,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L3,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L4,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L5,
        severity: "blocking (warn elsewhere)",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L6,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L7,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L8,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L9,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L10,
        severity: "blocking at ratified; warning at proposed",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L11,
        severity: "blocking / warning",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L12,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L13,
        severity: "blocking",
        stage: Stage::Resolution,
    },
    Rule {
        id: RuleId::L14,
        severity: "blocking",
        stage: Stage::Resolution,
    },
    Rule {
        id: RuleId::L15,
        severity: "blocking / warning",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L16,
        severity: "blocking",
        stage: Stage::Core,
    },
    Rule {
        id: RuleId::L17,
        severity: "blocking in sitting mode",
        stage: Stage::Resolution,
    },
    Rule {
        id: RuleId::L18,
        severity: "blocking",
        stage: Stage::Resolution,
    },
];

#[cfg(test)]
mod tests {
    use super::{RuleId, CATALOG};

    #[test]
    fn every_catalogued_id_round_trips_through_its_printed_form() {
        for rule in CATALOG {
            assert_eq!(RuleId::parse(rule.id.as_str()), Some(rule.id));
        }
        assert_eq!(RuleId::parse("L19"), None);
        assert_eq!(RuleId::parse("l1"), None);
    }

    #[test]
    fn the_catalog_holds_no_duplicate_id() {
        let mut ids: Vec<RuleId> = CATALOG.iter().map(|rule| rule.id).collect();
        ids.sort_unstable();
        let mut unique = ids.clone();
        unique.dedup();
        assert_eq!(ids, unique);
    }
}
