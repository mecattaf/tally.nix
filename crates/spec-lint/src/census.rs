//! `--census`: the oracle binding of every claim, enumerated.
//!
//! `specs/README.md` §6 is the whole rule — every claim and unchanged line
//! carries **exactly one** binding: a named flake check attribute, a witnessed
//! gate argv, or `[HUMAN-ATTENDED]`. Zero or two is `[L9]`. The census renders
//! the enumeration and reports the defects; it never grades. That is the point:
//! coverage stops being the judgment "is this tested anywhere" and becomes a
//! table an operator can read down.
//!
//! A gate binding is witnessed against the governing worklist, so `[gate: x]`
//! renders the argv that will actually run. Without a worklist the id renders
//! unwitnessed rather than wrong — a spec proposed before its sitting has no
//! worklist to resolve against.

use crate::artifacts::Worklist;
use crate::claim::{self, BindingKind};
use crate::defect::Defect;
use crate::document::Document;
use crate::index;
use crate::lint::Context;
use crate::rules::RuleId;

/// One census row: a claim id, the kind of oracle it binds, and the oracle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub claim: String,
    pub binding: String,
    pub oracle: String,
}

/// Enumerate the bindings of one spec, with the `[L9]` defects the enumeration
/// exposes.
pub fn census(
    document: &Document,
    context: &Context,
    worklist: Option<&Worklist>,
) -> (Vec<Row>, Vec<Defect>) {
    let mut rows = Vec::new();
    let mut defects = Vec::new();

    for entry in index::entries(document) {
        let bindings = claim::parse(&entry.body).bindings;
        let (binding, oracle) = match bindings.as_slice() {
            [] => {
                defects.push(Defect::blocking(
                    &context.file,
                    entry.line,
                    RuleId::L9,
                    format!(
                        "claim `{}` binds no oracle; the census admits one check attribute, one witnessed gate argv, or one [HUMAN-ATTENDED] mark",
                        entry.id
                    ),
                ));
                ("none".to_owned(), "—".to_owned())
            }
            [single] => match single.kind {
                BindingKind::HumanAttended => ("HUMAN-ATTENDED".to_owned(), "—".to_owned()),
                BindingKind::Check => ("check".to_owned(), single.value.clone()),
                BindingKind::Gate => (
                    "gate".to_owned(),
                    witness(&single.value, worklist, context, entry.line, &mut defects),
                ),
            },
            many => {
                defects.push(Defect::blocking(
                    &context.file,
                    entry.line,
                    RuleId::L9,
                    format!(
                        "claim `{}` binds {} oracles; the census admits exactly one",
                        entry.id,
                        many.len()
                    ),
                ));
                (
                    format!("{} bindings", many.len()),
                    many.iter()
                        .map(|binding| match binding.kind {
                            BindingKind::HumanAttended => "HUMAN-ATTENDED".to_owned(),
                            BindingKind::Check => format!("check: {}", binding.value),
                            BindingKind::Gate => format!("gate: {}", binding.value),
                        })
                        .collect::<Vec<String>>()
                        .join("; "),
                )
            }
        };
        rows.push(Row {
            claim: entry.id,
            binding,
            oracle,
        });
    }

    (rows, defects)
}

/// The argv a gate id resolves to in the governing worklist.
fn witness(
    id: &str,
    worklist: Option<&Worklist>,
    context: &Context,
    line: usize,
    defects: &mut Vec<Defect>,
) -> String {
    let Some(worklist) = worklist else {
        return format!("{id} (unwitnessed: no governing worklist at this revision)");
    };
    match worklist.gate(id) {
        Some(gate) => format!("{id}: {}", gate.argv.join(" ")),
        None => {
            defects.push(Defect::blocking(
                context.file.as_str(),
                line,
                RuleId::L9,
                format!("gate `{id}` resolves to no gate id in `{}`", worklist.file),
            ));
            format!("{id}: unresolved")
        }
    }
}

/// The census as the markdown table the mode prints.
pub fn render(rows: &[Row]) -> String {
    let mut table = String::from("| claim | binding | oracle |\n|---|---|---|\n");
    for row in rows {
        table.push_str(&format!(
            "| {} | {} | {} |\n",
            cell(&row.claim),
            cell(&row.binding),
            cell(&row.oracle)
        ));
    }
    table
}

/// A markdown cell never breaks its row: the pipe is escaped, the newline
/// cannot occur (a claim is one logical line), and an empty cell reads `—`.
fn cell(text: &str) -> String {
    if text.is_empty() {
        return "—".to_owned();
    }
    text.replace('|', "\\|")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{census, render};
    use crate::artifacts::Worklist;
    use crate::document::Document;
    use crate::lint::Context;

    const SPEC: &str = "# s — t\n\n## Claims\n\n### R1 — the group\nWhy: because.\n1.1 a → b. [gate: cargo-tests]\n1.2 c → d. [check: module-layer]\n1.3 e → f. [HUMAN-ATTENDED]\n1.4 g → h.\n1.5 i → j. [gate: cargo-tests] [check: module-layer]\n1.6 k → l. [gate: absent-gate]\n\n## Unchanged\n\nU.1 m → n. [gate: cargo-tests]\n";

    fn context() -> Context {
        Context {
            file: "spec.md".to_owned(),
            identity: "s".to_owned(),
            directory: PathBuf::from("."),
            root: PathBuf::from("."),
        }
    }

    fn worklist() -> Worklist {
        Worklist::parse(
            "w.json".to_owned(),
            r#"{"campaign": {"gates": [{"id": "cargo-tests", "argv": ["cargo", "test"]}]}}"#
                .to_owned(),
        )
    }

    #[test]
    fn the_census_enumerates_every_claim_and_unchanged_line() {
        let (rows, _) = census(&Document::parse(SPEC), &context(), Some(&worklist()));
        let claims: Vec<&str> = rows.iter().map(|row| row.claim.as_str()).collect();
        assert_eq!(claims, ["1.1", "1.2", "1.3", "1.4", "1.5", "1.6", "U.1"]);
        assert_eq!(rows[0].oracle, "cargo-tests: cargo test");
        assert_eq!(rows[1].binding, "check");
        assert_eq!(rows[2].binding, "HUMAN-ATTENDED");
    }

    #[test]
    fn zero_two_and_unresolvable_bindings_are_the_census_defects() {
        let (_, defects) = census(&Document::parse(SPEC), &context(), Some(&worklist()));
        let messages: Vec<String> = defects.iter().map(ToString::to_string).collect();
        assert_eq!(defects.len(), 3, "{messages:?}");
        assert!(messages.iter().any(|line| line.contains("binds no oracle")));
        assert!(messages.iter().any(|line| line.contains("binds 2 oracles")));
        assert!(messages
            .iter()
            .any(|line| line.contains("`absent-gate` resolves to no gate id")));
    }

    #[test]
    fn a_gate_without_a_worklist_renders_unwitnessed_and_reports_nothing() {
        let spec =
            "# s — t\n\n## Claims\n\n### R1 — g\nWhy: because.\n1.1 a → b. [gate: cargo-tests]\n";
        let (rows, defects) = census(&Document::parse(spec), &context(), None);
        assert!(defects.is_empty());
        assert!(rows[0].oracle.contains("unwitnessed"));
    }

    #[test]
    fn the_table_escapes_a_pipe_and_renders_an_empty_cell() {
        let (rows, _) = census(&Document::parse(SPEC), &context(), Some(&worklist()));
        let table = render(&rows);
        assert!(table.starts_with("| claim | binding | oracle |\n|---|---|---|\n"));
        assert!(table.contains("| 1.3 | HUMAN-ATTENDED | — |\n"));
        assert_eq!(table.lines().count(), rows.len() + 2);
    }
}
