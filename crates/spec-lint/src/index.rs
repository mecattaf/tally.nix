//! The join keys one `spec.md` exposes to the cross-artifact half: the ids of
//! its claim and unchanged lines, the areas its stages claim, and the anchors a
//! worklist pointer may cite.
//!
//! `specs/README.md` §3 fixes anchors as number-derived — `### R2 — the trace`
//! anchors at `#r2` and nowhere else, which is what makes a retitle safe — so
//! the anchor set is computed from the heading numbers, never from the titles.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use regex::Regex;

use crate::claim;
use crate::document::Document;

/// One arrow line, keyed by the id the trace joins on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// `1.2` for a claim line, `U.1` for an unchanged line.
    pub id: String,
    /// The claim group, absent on an unchanged line.
    pub group: Option<u32>,
    pub line: usize,
    /// Everything after the id token.
    pub body: String,
}

/// One `### S<n>` stage and the body it claims.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stage {
    pub number: u32,
    pub line: usize,
    pub body: String,
}

impl Stage {
    /// The claim groups the stage lists, `R1` and the ranges `R1–R4` alike.
    pub fn area(&self) -> BTreeSet<u32> {
        static SINGLE: OnceLock<Regex> = OnceLock::new();
        static RANGE: OnceLock<Regex> = OnceLock::new();
        let single = SINGLE.get_or_init(|| Regex::new(r"\bR([0-9]+)\b").expect("compiles"));
        let range = RANGE
            .get_or_init(|| Regex::new(r"\bR([0-9]+)\s*[–—-]\s*R?([0-9]+)\b").expect("compiles"));

        let mut groups: BTreeSet<u32> = BTreeSet::new();
        for captured in range.captures_iter(&self.body) {
            let (Ok(first), Ok(last)) = (captured[1].parse::<u32>(), captured[2].parse::<u32>())
            else {
                continue;
            };
            groups.extend(first.min(last)..=first.max(last));
        }
        for captured in single.captures_iter(&self.body) {
            if let Ok(number) = captured[1].parse::<u32>() {
                groups.insert(number);
            }
        }
        groups
    }
}

/// The claim lines of `## Claims`, then the arrow lines of `## Unchanged`, in
/// document order. A line whose id does not parse is a `[L3]`/`[L4]` defect the
/// check pass already reports; the join simply cannot key on it.
pub fn entries(document: &Document) -> Vec<Entry> {
    let mut entries = Vec::new();

    if let Some(section) = document.section("Claims") {
        for line in document.logical(section) {
            if line.trimmed().starts_with("###") {
                continue;
            }
            if let Some((group, index, body)) = claim::claim_id(line.trimmed()) {
                entries.push(Entry {
                    id: format!("{group}.{index}"),
                    group: Some(group),
                    line: line.number,
                    body: body.to_owned(),
                });
            }
        }
    }

    if let Some(section) = document.section("Unchanged") {
        for line in document.logical(section) {
            if let Some((index, body)) = claim::dotted_id(line.trimmed(), 'U') {
                entries.push(Entry {
                    id: format!("U.{index}"),
                    group: None,
                    line: line.number,
                    body: body.to_owned(),
                });
            }
        }
    }

    entries
}

/// The `### S<n>` stages of `## Stages`, each with the body under it.
pub fn stages(document: &Document) -> Vec<Stage> {
    static HEADING: OnceLock<Regex> = OnceLock::new();
    let heading = HEADING.get_or_init(|| Regex::new(r"^### S([0-9]+) — ").expect("compiles"));

    let Some(section) = document.section("Stages") else {
        return Vec::new();
    };
    let mut stages: Vec<Stage> = Vec::new();
    for line in document.logical(section) {
        match heading.captures(line.trimmed()) {
            Some(captured) => stages.push(Stage {
                number: captured[1].parse().unwrap_or_default(),
                line: line.number,
                body: String::new(),
            }),
            None => {
                if let Some(stage) = stages.last_mut() {
                    stage.body.push_str(line.trimmed());
                    stage.body.push('\n');
                }
            }
        }
    }
    stages
}

/// The anchors a markdown file offers a citation. `### R<n>`/`### S<n>` headings
/// anchor at their number only; every other heading anchors at its slug.
pub fn anchors(text: &str) -> BTreeSet<String> {
    static HEADING: OnceLock<Regex> = OnceLock::new();
    static NUMBERED: OnceLock<Regex> = OnceLock::new();
    let heading = HEADING.get_or_init(|| Regex::new(r"^#{1,6} +(.+?)\s*$").expect("compiles"));
    let numbered = NUMBERED.get_or_init(|| Regex::new(r"^([RS])([0-9]+) — ").expect("compiles"));

    let mut anchors = BTreeSet::new();
    let mut fenced = false;
    for raw in text.lines() {
        if raw.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let Some(captured) = heading.captures(raw) else {
            continue;
        };
        let title = &captured[1];
        if let Some(numbered) = numbered.captures(title) {
            anchors.insert(format!("{}{}", numbered[1].to_lowercase(), &numbered[2]));
        } else {
            anchors.insert(slug(title));
        }
    }
    anchors
}

/// The GitHub heading slug: lowercased, spaces hyphenated, everything else that
/// is not a letter, a digit, a hyphen, or an underscore dropped.
fn slug(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter_map(|character| match character {
            ' ' => Some('-'),
            '-' | '_' => Some(character),
            _ if character.is_alphanumeric() => Some(character),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{anchors, entries, slug, stages};
    use crate::document::Document;

    const SAMPLE: &str = "# id — title\n\n## Claims\n\n### R1 — the first\nWhy: because.\n1.1 a → b. [gate: g]\n1.2 c → d. [gate: g]\n\n### R2 — the second\nWhy: because.\n2.1 e → f. [gate: g]\n\n## Unchanged\n\nU.1 g → h. [gate: g]\n\n## Stages\n\n### S1 — built\nOrder: one-task. Claims R1; rulings Z1.\n\n### S2 — later\nUnauthored. Claims R2–R4.\n";

    #[test]
    fn every_claim_and_unchanged_line_becomes_a_join_key() {
        let ids: Vec<String> = entries(&Document::parse(SAMPLE))
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(ids, ["1.1", "1.2", "2.1", "U.1"]);
    }

    #[test]
    fn a_stage_area_expands_a_group_range() {
        let stages = stages(&Document::parse(SAMPLE));
        assert_eq!(stages.len(), 2);
        assert_eq!(stages[0].area(), [1].into_iter().collect());
        assert_eq!(stages[1].area(), [2, 3, 4].into_iter().collect());
    }

    #[test]
    fn anchors_derive_from_heading_numbers_and_section_slugs() {
        let found = anchors(SAMPLE);
        assert!(found.contains("r1"));
        assert!(found.contains("r2"));
        assert!(found.contains("s1"));
        assert!(found.contains("claims"));
        assert!(found.contains("unchanged"));
        assert!(!found.contains("r1--the-first"));
        assert_eq!(slug("The lint rule index"), "the-lint-rule-index");
    }

    #[test]
    fn a_heading_inside_a_fence_offers_no_anchor() {
        assert!(!anchors("# a — b\n\n```\n## Fenced\n```\n").contains("fenced"));
    }
}
