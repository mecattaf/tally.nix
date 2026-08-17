//! The protected set: repository paths no lane commit may carry.
//!
//! The set has three members, ruled at the eta seam sitting C1: the campaign's
//! worklist, the gate definitions, and every governing spec directory
//! `specs/<identity>/**`. The first two arrive protected. Gate definitions
//! live inside the worklist's own bytes, and the worklist is the arming act
//! itself — a push to the armed identity's worklist is what admits a new
//! epoch (`campaign_poll`), so those bytes are the operator's authority
//! surface by construction and a lane that rewrote them would be arming its
//! own campaign. The third member arrived with nothing: no lane wrote under
//! `specs/<identity>/` because task goals said so, which is authoring
//! discipline, not mechanism. This module is the mechanism; ownership
//! certification is where it bites, and a declared conflict domain does not
//! grant what it refuses.
//!
//! The rule is identity-blind on purpose. Ownership certification judges a
//! brief that names a task and a workspace and never names the armed
//! identity, so the deny list protects every governing spec directory rather
//! than one derived from a branch name or a campaign slug. A protection that
//! switches off when the name it guessed is wrong is not a protection — and
//! no lane has business writing another identity's governing spec either.
//!
//! Evidence is not exempt. A lane with evidence to land hands it to its final
//! message and the operator or the coordinator writes it: spec-directory
//! bytes move only by operator or coordinator hands.

use std::fmt;

/// The directory holding every identity's governing spec.
pub const SPECS_ROOT: &str = "specs";

/// The hands a protected path moves by, named the way the refusal names them.
pub const PROTECTED_SET_OWNER: &str = "operator or coordinator";

/// One member of the protected set, named the way a refusal names it.
///
/// Only the mechanically enforced member is modeled. The worklist and the
/// gate definitions inside it are protected upstream of any lane commit — the
/// operator's push is the arming act — so naming them here would invent a
/// second, weaker enforcement point for bytes that already have one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneProtection {
    /// `specs/<identity>/**`, the governing spec directory of one identity.
    GoverningSpec { identity: String },
}

impl LaneProtection {
    /// The protection's stable name, quoted verbatim by refusals so an
    /// operator reading a lane's failure can grep the rule that produced it.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::GoverningSpec { .. } => "governing-spec-directory",
        }
    }

    /// Who may move bytes this protection covers.
    #[must_use]
    pub fn owner(&self) -> &'static str {
        PROTECTED_SET_OWNER
    }
}

impl fmt::Display for LaneProtection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GoverningSpec { identity } => write!(
                formatter,
                "{}: the governing spec of identity {identity:?}",
                self.name()
            ),
        }
    }
}

/// Classify one repository-relative path against the protected set.
///
/// Path components are compared the way conflict domains are compared —
/// ASCII-case-insensitively — so a checkout on a case-folding filesystem
/// cannot spell its way around the deny list. Only a path *inside*
/// `specs/<identity>/` is protected: `specs/README.md` is the index over the
/// identities rather than any identity's governing spec, and lanes amend it.
#[must_use]
pub fn lane_protection(path: &str) -> Option<LaneProtection> {
    let mut components = path.split('/');
    if !components.next()?.eq_ignore_ascii_case(SPECS_ROOT) {
        return None;
    }
    let identity = components.next().filter(|piece| !piece.is_empty())?;
    // A third component is what makes this a write *under* the identity's
    // directory rather than to a file that merely shares its name.
    components.next().filter(|piece| !piece.is_empty())?;
    Some(LaneProtection::GoverningSpec {
        identity: identity.to_owned(),
    })
}

/// Every protected path among `paths`, paired with the protection it hit and
/// kept in the order given, so a refusal previews paths as the lane wrote them.
#[must_use]
pub fn protected_lane_paths<I, S>(paths: I) -> Vec<(String, LaneProtection)>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    paths
        .into_iter()
        .filter_map(|path| {
            let path = path.as_ref();
            lane_protection(path).map(|protection| (path.to_owned(), protection))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{lane_protection, protected_lane_paths, LaneProtection};

    #[test]
    fn deny_list_names_the_governing_spec_directory_and_its_identity() {
        let protection = lane_protection("specs/eta/spec.md").expect("protected");
        assert_eq!(
            protection,
            LaneProtection::GoverningSpec {
                identity: "eta".to_owned()
            }
        );
        assert_eq!(protection.name(), "governing-spec-directory");
        assert_eq!(protection.owner(), "operator or coordinator");
        assert_eq!(
            protection.to_string(),
            "governing-spec-directory: the governing spec of identity \"eta\""
        );
    }

    #[test]
    fn deny_list_covers_evidence_and_every_depth_under_an_identity() {
        for path in [
            "specs/eta/evidence/sitting-c1.md",
            "specs/zeta/contracts/claim-line.fixtures.json",
            "specs/001-toy/tasks.json",
        ] {
            assert!(
                lane_protection(path).is_some(),
                "the deny list must protect {path}"
            );
        }
    }

    #[test]
    fn deny_list_admits_paths_that_are_not_a_governing_spec_directory() {
        for path in [
            "crates/tally-core/src/campaign_protection.rs",
            "specs/README.md",
            "specs",
            "specs/eta",
            "crates/spec-lint/tests/fixtures/golden/specs/eta/spec.md",
            "",
        ] {
            assert_eq!(
                lane_protection(path),
                None,
                "the deny list must admit {path}"
            );
        }
    }

    #[test]
    fn deny_list_folds_case_the_way_conflict_domains_do() {
        assert_eq!(
            lane_protection("SPECS/Eta/spec.md"),
            Some(LaneProtection::GoverningSpec {
                identity: "Eta".to_owned()
            }),
            "a case-folding checkout must not spell its way past the deny list"
        );
    }

    #[test]
    fn deny_list_previews_protected_paths_in_the_order_written() {
        let protected = protected_lane_paths([
            "crates/tally-core/src/lib.rs",
            "specs/eta/evidence/sitting-c2.md",
            "specs/README.md",
            "specs/eta/spec.md",
        ]);
        assert_eq!(
            protected
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>(),
            ["specs/eta/evidence/sitting-c2.md", "specs/eta/spec.md"]
        );
        assert!(protected
            .iter()
            .all(|(_, protection)| protection.name() == "governing-spec-directory"));
    }
}
