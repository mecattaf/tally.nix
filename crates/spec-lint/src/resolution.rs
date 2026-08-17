//! The cross-artifact pass: the half of `specs/README.md` §7 that no single
//! `spec.md` can answer for.
//!
//! §1 fixes the layer's whole claim as one chain — spec → worklist → receipts →
//! release — and every link committed bytes. A pointer that does not resolve
//! breaks the chain silently, which is the 48-phantom-pointer class checked by
//! eye until now. This pass resolves it mechanically:
//!
//! - `[L13]` every `specs/**` pointer in the governing worklist names a file
//!   that exists, and, where the pointer carries an anchor, a number-derived
//!   anchor that file actually offers; every evidence path a trace row cites
//!   resolves the same way.
//! - `[L14]` `trace.json` validates against its committed schema, its rows name
//!   claims the spec declares, tasks the worklist declares, and acceptance ids
//!   those tasks declare; no release row precedes its sitting row; and every
//!   claim is traced to a task or left to an unauthored stage.
//! - `[L18]` every path a task's acceptance criteria require it to write falls
//!   inside the `conflictDomains` that task declares. The join is the one the
//!   lane discovers by being refused mid-flight; read at authoring time it is
//!   two committed fields of one file.
//!
//! Both halves are conditional on their artifact existing. A spec proposed
//! before its boundary sitting governs no worklist and has no trace rows yet;
//! absence is a lifecycle state, not a defect.

use std::collections::BTreeSet;

use crate::artifacts::{self, Artifacts, Trace, Worklist};
use crate::boundary;
use crate::defect::Defect;
use crate::document::Document;
use crate::index;
use crate::lint::Context;
use crate::rules::RuleId;
use crate::schema;
use crate::tree::Tree;

/// Resolve one identity directory against the worklist and the trace beside it.
pub fn resolve(document: &Document, context: &Context, artifacts: &Artifacts) -> Vec<Defect> {
    let tree = Tree::new(context.root.clone()).with_local(context.directory.clone());
    let mut defects = Vec::new();

    if let Some(worklist) = &artifacts.worklist {
        pointers(worklist, &tree, &mut defects);
        acceptance_domains(worklist, &mut defects);
    }
    if let Some(trace) = &artifacts.trace {
        validate(trace, &mut defects);
        declared_spec(trace, context, &mut defects);
        rows(
            document,
            trace,
            artifacts.worklist.as_ref(),
            &tree,
            &mut defects,
        );
    }
    if let (Some(worklist), Some(trace)) = (&artifacts.worklist, &artifacts.trace) {
        untraced(document, context, worklist, trace, &mut defects);
    }

    defects
}

/// `[L13]` — every `specs/**` pointer the governing worklist carries resolves,
/// anchor included.
fn pointers(worklist: &Worklist, tree: &Tree, defects: &mut Vec<Defect>) {
    for task in &worklist.tasks {
        for pointer in &task.spec_sections {
            if !pointer.starts_with("specs/") {
                continue;
            }
            let line = artifacts::line_of(&worklist.text, pointer);
            let (path, anchor) = split_anchor(pointer);
            if !tree.exists(path) {
                defects.push(Defect::blocking(
                    &worklist.file,
                    line,
                    RuleId::L13,
                    format!(
                        "task `{}` reads first from `{pointer}`, which does not exist at this revision",
                        task.id
                    ),
                ));
                continue;
            }
            let Some(anchor) = anchor else {
                continue;
            };
            // A directory resolves as a pointer and offers no anchor: the
            // anchor set of what cannot be read as text is empty, not skipped.
            let offered = tree.read(path).map(|bytes| index::anchors(&bytes));
            if !offered.is_some_and(|offered| offered.contains(anchor)) {
                defects.push(Defect::blocking(
                    &worklist.file,
                    line,
                    RuleId::L13,
                    format!(
                        "task `{}` reads first from `{pointer}`; `{path}` offers no `#{anchor}` anchor",
                        task.id
                    ),
                ));
            }
        }
    }
}

/// `[L18]` — every path a task's acceptance criteria require the lane to write
/// is inside the write boundary that task declares.
///
/// A task whose `conflictDomains` key is absent declares no boundary here: the
/// serial-task boundary is inferred after execution and the checkpoint kind
/// carries none, so there is no allowlist to hold a path against. A declared
/// but empty boundary grants nothing, and is read as the author wrote it.
fn acceptance_domains(worklist: &Worklist, defects: &mut Vec<Defect>) {
    for task in &worklist.tasks {
        let Some(domains) = &task.conflict_domains else {
            continue;
        };
        for criterion in &task.criteria {
            for path in boundary::write_targets(&criterion.argv) {
                if domains.iter().any(|domain| boundary::inside(&path, domain)) {
                    continue;
                }
                defects.push(Defect::blocking(
                    &worklist.file,
                    artifacts::line_of(&worklist.text, &path),
                    RuleId::L18,
                    format!(
                        "task `{}` acceptance `{}` writes `{path}`, which the conflictDomains that task declares do not grant",
                        task.id, criterion.id
                    ),
                ));
            }
        }
    }
}

/// `[L14]` — the trace against its committed schema. Without a schema in the
/// tree there is no oracle, and a missing oracle is reported rather than passed.
fn validate(trace: &Trace, defects: &mut Vec<Defect>) {
    let Some((file, schema)) = &trace.schema else {
        defects.push(Defect::blocking(
            &trace.file,
            1,
            RuleId::L14,
            "no `contracts/trace.schema.json` resolves for this identity; the trace has no byte oracle",
        ));
        return;
    };
    match schema::validate(schema, &trace.json) {
        Err(error) => defects.push(Defect::blocking(
            &trace.file,
            1,
            RuleId::L14,
            format!("{file} cannot be evaluated: {error}"),
        )),
        Ok(failures) => {
            for failure in failures {
                defects.push(Defect::blocking(
                    &trace.file,
                    pointer_line(trace, &failure.at),
                    RuleId::L14,
                    format!(
                        "{} is invalid against {file}: {}",
                        failure.at, failure.message
                    ),
                ));
            }
        }
    }
}

/// `[L14]` — the trace names the spec it sits beside. Identity is the join key
/// `specs/README.md` §2 fixes; a trace pointing elsewhere joins nothing.
fn declared_spec(trace: &Trace, context: &Context, defects: &mut Vec<Defect>) {
    let expected = format!("specs/{}/spec.md", context.identity);
    if trace.spec != expected {
        defects.push(Defect::blocking(
            &trace.file,
            artifacts::line_of(&trace.text, "\"spec\""),
            RuleId::L14,
            format!(
                "the trace names `{}`; it sits beside `{expected}`",
                trace.spec
            ),
        ));
    }
}

/// `[L14]`/`[L13]` — every row resolves: its claim in the spec, its task and
/// acceptance ids in the worklist, its evidence in the tree, and a release row
/// only after the sitting row it closes.
fn rows(
    document: &Document,
    trace: &Trace,
    worklist: Option<&Worklist>,
    tree: &Tree,
    defects: &mut Vec<Defect>,
) {
    let declared: BTreeSet<String> = index::entries(document)
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    let mut sat: BTreeSet<(String, String)> = BTreeSet::new();

    let mut ordered: Vec<&artifacts::Row> = trace.rows.iter().collect();
    ordered.sort_by_key(|row| row.seq);

    for row in ordered {
        if !declared.contains(&row.claim) {
            defects.push(Defect::blocking(
                &trace.file,
                row.line,
                RuleId::L14,
                format!(
                    "row {} traces claim `{}`, which `{}` does not declare",
                    row.seq, row.claim, trace.spec
                ),
            ));
        }

        match worklist {
            None => {}
            Some(worklist) => match worklist.task(&row.task) {
                None => defects.push(Defect::blocking(
                    &trace.file,
                    row.line,
                    RuleId::L14,
                    format!(
                        "row {} traces task `{}`, which `{}` does not declare",
                        row.seq, row.task, worklist.file
                    ),
                )),
                Some(task) => {
                    for id in &row.acceptance {
                        if !task.declares(id) {
                            defects.push(Defect::blocking(
                                &trace.file,
                                row.line,
                                RuleId::L14,
                                format!(
                                    "row {} cites acceptance id `{id}`, which task `{}` does not declare",
                                    row.seq, task.id
                                ),
                            ));
                        }
                    }
                }
            },
        }

        for citation in &row.evidence {
            let (path, anchor) = split_anchor(citation);
            if !tree.exists(path) {
                defects.push(Defect::blocking(
                    &trace.file,
                    row.line,
                    RuleId::L13,
                    format!(
                        "row {} cites evidence `{citation}`, which does not exist at this revision",
                        row.seq
                    ),
                ));
                continue;
            }
            // An evidence id names a finding inside the ledger — `F38`, `E1` —
            // so the id resolves against the file's bytes as well as against
            // the anchors its headings derive.
            let Some(anchor) = anchor else {
                continue;
            };
            let bytes = tree.read(path).unwrap_or_default();
            if !bytes.contains(anchor) && !index::anchors(&bytes).contains(anchor) {
                defects.push(Defect::blocking(
                    &trace.file,
                    row.line,
                    RuleId::L13,
                    format!(
                        "row {} cites evidence `{citation}`; `{path}` carries no `{anchor}`",
                        row.seq
                    ),
                ));
            }
        }

        let pair = (row.claim.clone(), row.task.clone());
        match row.kind.as_str() {
            "sitting" => {
                sat.insert(pair);
            }
            "release" if !sat.contains(&pair) => defects.push(Defect::blocking(
                &trace.file,
                row.line,
                RuleId::L14,
                format!(
                    "row {} releases claim `{}` on task `{}` with no prior sitting row",
                    row.seq, row.claim, row.task
                ),
            )),
            _ => {}
        }
    }
}

/// `[L14]` — every claim is traced to a task or left to an unauthored stage.
/// Coverage is an enumeration: a claim in neither place is a claim nobody owns.
fn untraced(
    document: &Document,
    context: &Context,
    worklist: &Worklist,
    trace: &Trace,
    defects: &mut Vec<Defect>,
) {
    let traced: BTreeSet<&str> = trace.rows.iter().map(|row| row.claim.as_str()).collect();
    let unauthored = unauthored_area(document, worklist);

    for entry in index::entries(document) {
        let Some(group) = entry.group else {
            continue;
        };
        if traced.contains(entry.id.as_str()) || unauthored.contains(&group) {
            continue;
        }
        defects.push(Defect::blocking(
            &context.file,
            entry.line,
            RuleId::L14,
            format!(
                "claim `{}` is traced to no task in `{}` and sits under no unauthored stage",
                entry.id, trace.file
            ),
        ));
    }
}

/// The claim groups the unauthored stages claim. A stage that names a task the
/// governing worklist declares is authored; its claims are owed trace rows.
fn unauthored_area(document: &Document, worklist: &Worklist) -> BTreeSet<u32> {
    let mut area = BTreeSet::new();
    for stage in index::stages(document) {
        if worklist
            .tasks
            .iter()
            .any(|task| !task.id.is_empty() && stage.body.contains(&task.id))
        {
            continue;
        }
        area.extend(stage.area());
    }
    area
}

/// Split a citation into its path and its optional anchor.
fn split_anchor(citation: &str) -> (&str, Option<&str>) {
    match citation.split_once('#') {
        Some((path, anchor)) if !anchor.is_empty() => (path, Some(anchor)),
        _ => (citation.split('#').next().unwrap_or_default(), None),
    }
}

/// The line a schema failure's instance pointer lands on. A failure inside
/// `/rows/<n>` anchors on that row, which is the byte an operator has to
/// change; anything else anchors on the member the pointer ends with.
fn pointer_line(trace: &Trace, at: &str) -> usize {
    let segments: Vec<&str> = at.split('/').skip(1).collect();
    if let [rows, index, ..] = segments.as_slice() {
        if *rows == "rows" {
            if let Some(row) = index
                .parse::<usize>()
                .ok()
                .and_then(|at| trace.rows.get(at))
            {
                return row.line;
            }
        }
    }
    match segments.last() {
        Some(last) if !last.is_empty() && last.parse::<usize>().is_err() => {
            artifacts::line_of(&trace.text, &format!("\"{last}\""))
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{split_anchor, unauthored_area};
    use crate::artifacts::Worklist;
    use crate::document::Document;

    #[test]
    fn an_anchor_splits_off_its_path() {
        assert_eq!(
            split_anchor("specs/zeta/spec.md#r2"),
            ("specs/zeta/spec.md", Some("r2"))
        );
        assert_eq!(split_anchor("specs/README.md"), ("specs/README.md", None));
        assert_eq!(split_anchor("specs/README.md#"), ("specs/README.md", None));
    }

    #[test]
    fn a_stage_naming_a_worklist_task_is_authored_and_excuses_nothing() {
        let document = Document::parse(
            "# a — b\n\n## Stages\n\n### S1 — built\nOrder: a-task. Claims R1.\n\n### S2 — later\nUnauthored. Claims R2.\n",
        );
        let worklist = Worklist::parse(
            "w.json".to_owned(),
            "{\"tasks\": [{\"id\": \"a-task\"}]}".to_owned(),
        );
        let area = unauthored_area(&document, &worklist);
        assert!(!area.contains(&1));
        assert!(area.contains(&2));
    }
}
