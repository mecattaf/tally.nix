//! `--coverage`: the claim ↔ task ↔ acceptance-id ↔ evidence join, rendered.
//!
//! This is the close-out proof. The operator hands the table to
//! `tally campaign release` verbatim as the campaign's intent, which is only
//! honest if the table is a rendering rather than a retelling: every cell comes
//! from `trace.json` and the spec beside it, and two runs over the same bytes
//! produce the same bytes. Nothing here reads a clock, a receipt, or a
//! directory listing, so byte-stability is a property of the code and not a
//! habit of the caller.
//!
//! Every claim the spec declares appears, traced or not. A coverage table that
//! only shows what is covered is the judgment the census exists to delete.

use std::collections::BTreeMap;

use crate::artifacts::Trace;
use crate::document::Document;
use crate::index;

/// One rendered row of the join: one claim ↔ task pair, or one untraced claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub claim: String,
    pub task: String,
    pub acceptance: Vec<String>,
    pub evidence: Vec<String>,
}

/// One claim ↔ task pair while it is still being gathered: the earliest row
/// that named the pair, and what every row on it contributed.
#[derive(Clone, Debug, Default)]
struct Gathered {
    first: i64,
    acceptance: Vec<String>,
    evidence: Vec<String>,
}

/// Join the spec's claims to the trace rows that carry them.
pub fn coverage(document: &Document, trace: Option<&Trace>) -> Vec<Row> {
    let mut rows = Vec::new();

    for entry in index::entries(document) {
        // Keyed by task, so the gather never depends on a hash order; ordered
        // afterwards by the earliest row that named the pair, so the render
        // follows the trace's own append order.
        let mut by_task: BTreeMap<String, Gathered> = BTreeMap::new();
        for row in trace.into_iter().flat_map(|trace| &trace.rows) {
            if row.claim != entry.id {
                continue;
            }
            let gathered = by_task.entry(row.task.clone()).or_insert(Gathered {
                first: row.seq,
                ..Gathered::default()
            });
            gathered.first = gathered.first.min(row.seq);
            extend(&mut gathered.acceptance, row.acceptance.iter());
            extend(&mut gathered.evidence, row.evidence.iter());
            if !row.witness.is_empty() {
                extend(&mut gathered.evidence, std::iter::once(&row.witness));
            }
        }

        if by_task.is_empty() {
            rows.push(Row {
                claim: entry.id,
                task: String::new(),
                acceptance: Vec::new(),
                evidence: Vec::new(),
            });
            continue;
        }

        let mut joined: Vec<(String, Gathered)> = by_task.into_iter().collect();
        joined.sort_by(|left, right| {
            left.1
                .first
                .cmp(&right.1.first)
                .then_with(|| left.0.cmp(&right.0))
        });
        for (task, gathered) in joined {
            rows.push(Row {
                claim: entry.id.clone(),
                task,
                acceptance: gathered.acceptance,
                evidence: gathered.evidence,
            });
        }
    }

    rows
}

/// The join as the markdown table the mode prints.
pub fn render(rows: &[Row]) -> String {
    let mut table = String::from("| claim | task | acceptance | evidence |\n|---|---|---|---|\n");
    for row in rows {
        table.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            cell(&row.claim),
            cell(&row.task),
            cell(&row.acceptance.join(", ")),
            cell(&row.evidence.join(", "))
        ));
    }
    table
}

fn extend<'a>(into: &mut Vec<String>, items: impl Iterator<Item = &'a String>) {
    for item in items {
        if !into.contains(item) {
            into.push(item.clone());
        }
    }
}

fn cell(text: &str) -> String {
    if text.is_empty() {
        return "—".to_owned();
    }
    text.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use super::{coverage, render};
    use crate::artifacts::Trace;
    use crate::document::Document;

    const SPEC: &str = "# s — t\n\n## Claims\n\n### R1 — the group\nWhy: because.\n1.1 a → b. [gate: g]\n1.2 c → d. [gate: g]\n\n## Unchanged\n\nU.1 e → f. [gate: g]\n";

    const TRACE: &str = r#"{ "schemaVersion": 1, "spec": "specs/s/spec.md", "rows": [
      { "seq": 2, "kind": "sitting", "claim": "1.1", "task": "second", "acceptance": ["b"] },
      { "seq": 1, "kind": "sitting", "claim": "1.1", "task": "first",
        "acceptance": ["a"], "evidence": ["specs/s/evidence/note.md"] },
      { "seq": 3, "kind": "release", "claim": "1.1", "task": "first",
        "merged": "0000000000000000000000000000000000000000",
        "witness": "summary/complete", "release": "v0.1.0" }
    ] }"#;

    fn trace() -> Trace {
        Trace::parse("t.json".to_owned(), TRACE.to_owned(), None)
    }

    #[test]
    fn the_join_is_one_row_per_claim_task_pair_and_names_untraced_claims() {
        let rows = coverage(&Document::parse(SPEC), Some(&trace()));
        let pairs: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.claim.as_str(), row.task.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("1.1", "first"),
                ("1.1", "second"),
                ("1.2", ""),
                ("U.1", "")
            ]
        );
        assert_eq!(rows[0].acceptance, ["a"]);
        assert_eq!(
            rows[0].evidence,
            ["specs/s/evidence/note.md", "summary/complete"]
        );
    }

    #[test]
    fn the_same_bytes_render_the_same_bytes() {
        let document = Document::parse(SPEC);
        let first = render(&coverage(&document, Some(&trace())));
        let second = render(&coverage(&Document::parse(SPEC), Some(&trace())));
        assert_eq!(first, second);
        assert!(first.starts_with("| claim | task | acceptance | evidence |\n"));
        assert!(first.contains("| 1.2 | — | — | — |\n"));
    }

    #[test]
    fn a_spec_with_no_trace_still_enumerates_its_claims() {
        let rows = coverage(&Document::parse(SPEC), None);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|row| row.task.is_empty()));
    }
}
