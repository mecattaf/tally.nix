//! One defect is one stderr line: `<file>:<line>: <rule-id>: <message>`.

use std::fmt;

use crate::rules::RuleId;

/// Warnings let a run finish at exit 1; blocking defects stop it at exit 2.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Severity {
    Warning,
    Blocking,
}

/// A single rule violation, anchored to the line that carries it.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Defect {
    pub line: usize,
    pub rule: RuleId,
    pub severity: Severity,
    pub file: String,
    pub message: String,
}

impl Defect {
    /// A defect that fails the lint.
    pub fn blocking(file: &str, line: usize, rule: RuleId, message: impl Into<String>) -> Self {
        Self {
            line,
            rule,
            severity: Severity::Blocking,
            file: file.to_owned(),
            message: message.into(),
        }
    }

    /// A defect that reports without failing.
    pub fn warning(file: &str, line: usize, rule: RuleId, message: impl Into<String>) -> Self {
        Self {
            line,
            rule,
            severity: Severity::Warning,
            file: file.to_owned(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Defect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}: {}: {}",
            self.file, self.line, self.rule, self.message
        )
    }
}

/// What a whole run amounts to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Clean,
    Warnings,
    Blocking,
}

impl Outcome {
    /// The outcome of a defect list.
    pub fn of(defects: &[Defect]) -> Self {
        if defects
            .iter()
            .any(|defect| defect.severity == Severity::Blocking)
        {
            Self::Blocking
        } else if defects.is_empty() {
            Self::Clean
        } else {
            Self::Warnings
        }
    }

    /// The process exit code: 0 clean, 1 warnings only, 2 blocking.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::Clean => 0,
            Self::Warnings => 1,
            Self::Blocking => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Defect, Outcome, Severity};
    use crate::rules::RuleId;

    #[test]
    fn a_defect_prints_one_line_in_the_contract_shape() {
        let defect = Defect::blocking("specs/zeta/spec.md", 61, RuleId::L7, "unsourced numeral 30");
        assert_eq!(
            defect.to_string(),
            "specs/zeta/spec.md:61: L7: unsourced numeral 30"
        );
    }

    #[test]
    fn the_outcome_ranks_blocking_over_warning_over_clean() {
        let warning = Defect::warning("spec.md", 1, RuleId::L11, "defined, never used");
        let blocking = Defect::blocking("spec.md", 2, RuleId::L1, "section out of order");

        assert_eq!(Outcome::of(&[]), Outcome::Clean);
        assert_eq!(
            Outcome::of(std::slice::from_ref(&warning)),
            Outcome::Warnings
        );
        assert_eq!(Outcome::of(&[warning, blocking]), Outcome::Blocking);
        assert_eq!(Outcome::Clean.exit_code(), 0);
        assert_eq!(Outcome::Warnings.exit_code(), 1);
        assert_eq!(Outcome::Blocking.exit_code(), 2);
        assert!(Severity::Blocking > Severity::Warning);
    }
}
