//! The two cross-artifact files a spec is joined to: the governing worklist
//! `silent-factory-worklists/<identity>.json` and the append-only
//! `trace.json` beside the spec.
//!
//! Both readers are deliberately lenient about shape. The worklist has its own
//! admitting authority (constitution A2) and the trace has a committed schema;
//! a missing or mistyped field here becomes a defect line, never a panic and
//! never a read that stops the rest of the pass.

use std::path::Path;

use anyhow::Context as _;
use serde_json::Value;

use crate::lint::Context;

/// One acceptance criterion of a task: the id a trace row cites and the argv
/// the criterion is graded by.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Criterion {
    pub id: String,
    pub argv: Vec<String>,
}

/// One task of the governing worklist, reduced to the fields the join reads.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Task {
    pub id: String,
    /// `readFirst.specSections`, verbatim.
    pub spec_sections: Vec<String>,
    /// The `acceptanceCriteria`, in worklist order.
    pub criteria: Vec<Criterion>,
    /// `conflictDomains` — the write boundary the task declares. `None` when
    /// the key is absent: a boundary inferred after execution declares no
    /// allowlist here, and the checkpoint kind carries none by contract.
    pub conflict_domains: Option<Vec<String>>,
}

impl Task {
    /// Whether the task declares an acceptance id.
    pub fn declares(&self, acceptance: &str) -> bool {
        self.criteria
            .iter()
            .any(|criterion| criterion.id == acceptance)
    }
}

/// One campaign gate, so a `[gate: <id>]` binding can be witnessed by its argv.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Gate {
    pub id: String,
    pub argv: Vec<String>,
}

/// The governing worklist as the join reads it.
#[derive(Clone, Debug)]
pub struct Worklist {
    /// The path printed on a defect line.
    pub file: String,
    /// The bytes, kept so a defect can name the line a pointer sits on.
    pub text: String,
    pub tasks: Vec<Task>,
    pub gates: Vec<Gate>,
}

impl Worklist {
    pub(crate) fn parse(file: String, text: String) -> Self {
        let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let tasks = json["tasks"]
            .as_array()
            .map(|tasks| {
                tasks
                    .iter()
                    .map(|task| Task {
                        id: string(&task["id"]),
                        spec_sections: strings(&task["readFirst"]["specSections"]),
                        criteria: task["acceptanceCriteria"]
                            .as_array()
                            .map(|criteria| {
                                criteria
                                    .iter()
                                    .map(|entry| Criterion {
                                        id: string(&entry["id"]),
                                        argv: strings(&entry["argv"]),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        conflict_domains: task["conflictDomains"]
                            .as_array()
                            .map(|domains| domains.iter().map(string).collect()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let gates = json["campaign"]["gates"]
            .as_array()
            .map(|gates| {
                gates
                    .iter()
                    .map(|gate| Gate {
                        id: string(&gate["id"]),
                        argv: strings(&gate["argv"]),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            file,
            text,
            tasks,
            gates,
        }
    }

    pub fn task(&self, id: &str) -> Option<&Task> {
        self.tasks.iter().find(|task| task.id == id)
    }

    pub fn gate(&self, id: &str) -> Option<&Gate> {
        self.gates.iter().find(|gate| gate.id == id)
    }
}

/// One `trace.json` row, reduced to the fields the join reads.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Row {
    pub seq: i64,
    /// `sitting` or `release`.
    pub kind: String,
    pub claim: String,
    pub task: String,
    pub acceptance: Vec<String>,
    pub evidence: Vec<String>,
    /// The release row's witness ref; empty on a sitting row.
    pub witness: String,
    /// The line the row opens on, for the defect anchor.
    pub line: usize,
}

/// The trace file as the join reads it, plus the raw value the schema validates.
#[derive(Clone, Debug)]
pub struct Trace {
    pub file: String,
    pub text: String,
    pub json: Value,
    /// The `spec` pointer the file declares.
    pub spec: String,
    pub rows: Vec<Row>,
    /// The schema the file is validated against, when the tree carries one.
    pub schema: Option<(String, Value)>,
}

impl Trace {
    pub(crate) fn parse(file: String, text: String, schema: Option<(String, Value)>) -> Self {
        let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let rows = json["rows"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| Row {
                        seq: row["seq"].as_i64().unwrap_or_default(),
                        kind: string(&row["kind"]),
                        claim: string(&row["claim"]),
                        task: string(&row["task"]),
                        acceptance: strings(&row["acceptance"]),
                        evidence: strings(&row["evidence"]),
                        witness: string(&row["witness"]),
                        line: 0,
                    })
                    .collect::<Vec<Row>>()
            })
            .unwrap_or_default();
        let rows = rows
            .into_iter()
            .map(|mut row| {
                row.line = row_line(&text, row.seq);
                row
            })
            .collect();
        Self {
            spec: string(&json["spec"]),
            file,
            text,
            json,
            rows,
            schema,
        }
    }
}

/// The cross-artifact files an identity directory is joined through. Either may
/// be absent: a spec proposed before its sitting governs no worklist yet, and
/// `trace.json` is written at the sitting that derives the first stage.
#[derive(Clone, Debug, Default)]
pub struct Artifacts {
    pub worklist: Option<Worklist>,
    pub trace: Option<Trace>,
}

impl Artifacts {
    /// Read whichever of the two files the tree carries. `worklist` overrides
    /// the `<root>/silent-factory-worklists/<identity>.json` convention.
    pub fn open(context: &Context, worklist: Option<&Path>) -> anyhow::Result<Self> {
        let worklist_path = worklist.map_or_else(
            || {
                context
                    .root
                    .join("silent-factory-worklists")
                    .join(format!("{}.json", context.identity))
            },
            Path::to_path_buf,
        );
        let worklist = read(&worklist_path)?
            .map(|text| Worklist::parse(worklist_path.display().to_string(), text));

        let trace_path = context.directory.join("trace.json");
        let trace = read(&trace_path)?
            .map(|text| Trace::parse(trace_path.display().to_string(), text, schema(context)));

        Ok(Self { worklist, trace })
    }
}

/// The trace schema: the copy beside the spec first, then the one
/// `specs/zeta/contracts/` carries for the whole layer.
fn schema(context: &Context) -> Option<(String, Value)> {
    let candidates = [
        context.directory.join("contracts/trace.schema.json"),
        context.root.join("specs/zeta/contracts/trace.schema.json"),
    ];
    candidates.into_iter().find_map(|path| {
        let text = std::fs::read_to_string(&path).ok()?;
        let json = serde_json::from_str(&text).ok()?;
        Some((path.display().to_string(), json))
    })
}

fn read(path: &Path) -> anyhow::Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }
    std::fs::read_to_string(path)
        .map(Some)
        .with_context(|| format!("cannot read {}", path.display()))
}

fn string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| items.iter().map(string).collect())
        .unwrap_or_default()
}

/// The 1-based line a needle first appears on, or 1 when it does not. A defect
/// about a JSON file still points at the byte the operator has to change.
pub fn line_of(text: &str, needle: &str) -> usize {
    text.lines()
        .position(|line| line.contains(needle))
        .map_or(1, |index| index + 1)
}

/// The line a row's `"seq"` member sits on, read past whatever spacing the file
/// was written with.
fn row_line(text: &str, seq: i64) -> usize {
    let needle = format!("\"seq\":{seq}");
    for (index, line) in text.lines().enumerate() {
        let squeezed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        let Some(at) = squeezed.find(&needle) else {
            continue;
        };
        if !squeezed[at + needle.len()..]
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
        {
            return index + 1;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::{line_of, Trace, Worklist};

    const WORKLIST: &str = r#"{
      "schemaVersion": 1,
      "campaign": { "name": "sample", "gates": [
        { "kind": "command", "id": "cargo-tests", "argv": ["cargo", "test"] }
      ] },
      "tasks": [
        { "id": "a-task",
          "readFirst": { "specSections": ["specs/sample/spec.md#r1"], "styleReferences": [] },
          "acceptanceCriteria": [ { "id": "green", "argv": ["true"] } ],
          "conflictDomains": ["crates/sample"] }
      ]
    }"#;

    const TRACE: &str = r#"{
      "schemaVersion": 1,
      "spec": "specs/sample/spec.md",
      "rows": [
        { "seq": 1, "at": "2026-08-15T18:45:00Z", "kind": "sitting", "claim": "1.1",
          "task": "a-task", "sitting": "sample/s1", "acceptance": ["green"] }
      ]
    }"#;

    #[test]
    fn a_worklist_reduces_to_its_tasks_gates_and_pointers() {
        let worklist = Worklist::parse("w.json".to_owned(), WORKLIST.to_owned());
        let task = worklist.task("a-task").expect("the task is read");
        assert_eq!(task.spec_sections, ["specs/sample/spec.md#r1"]);
        assert_eq!(task.criteria.len(), 1);
        assert_eq!(task.criteria[0].id, "green");
        assert_eq!(task.criteria[0].argv, ["true"]);
        assert!(task.declares("green"));
        assert!(!task.declares("absent"));
        assert_eq!(
            task.conflict_domains.as_deref(),
            Some(["crates/sample".to_owned()].as_slice())
        );
        assert_eq!(
            worklist.gate("cargo-tests").expect("the gate is read").argv,
            ["cargo", "test"]
        );
        assert!(worklist.gate("absent").is_none());
    }

    #[test]
    fn a_trace_reduces_to_its_rows_and_keeps_their_line_numbers() {
        let trace = Trace::parse("t.json".to_owned(), TRACE.to_owned(), None);
        assert_eq!(trace.spec, "specs/sample/spec.md");
        assert_eq!(trace.rows.len(), 1);
        assert_eq!(trace.rows[0].claim, "1.1");
        assert_eq!(trace.rows[0].task, "a-task");
        assert_eq!(trace.rows[0].acceptance, ["green"]);
        assert_eq!(trace.rows[0].line, 5);
    }

    #[test]
    fn a_malformed_file_reads_as_empty_rather_than_panicking() {
        let worklist = Worklist::parse("w.json".to_owned(), "not json".to_owned());
        assert!(worklist.tasks.is_empty());
        // An omitted boundary is absence, not an empty allowlist: the two mean
        // different things to the join, so the reader keeps them apart.
        let boundless = Worklist::parse(
            "w.json".to_owned(),
            "{\"tasks\": [{\"id\": \"a-checkpoint\"}]}".to_owned(),
        );
        assert_eq!(
            boundless
                .task("a-checkpoint")
                .expect("the task is read")
                .conflict_domains,
            None
        );
        let trace = Trace::parse("t.json".to_owned(), "{\"rows\": 7}".to_owned(), None);
        assert!(trace.rows.is_empty());
        assert_eq!(line_of("a\nb\nc", "zzz"), 1);
    }
}
