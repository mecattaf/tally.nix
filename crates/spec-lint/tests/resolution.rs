//! The cross-artifact half, proved the way the single-spec half is: one clean
//! tree the pass is silent over, and one deliberate break per rule class.
//!
//! The clean tree is `tests/fixtures/joined/` — a miniature working tree, not a
//! directory, because a join has nothing to resolve against without a root. It
//! carries the governing worklist, the spec, the trace, the evidence ledger,
//! and the trace contract, so these tests run wherever the crate builds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use spec_lint::defect::{Defect, Outcome};
use spec_lint::lint::{self, Options};
use spec_lint::rules::RuleId;
use spec_lint::{census, coverage};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The identity directory of the clean joined tree.
fn joined() -> PathBuf {
    fixtures().join("joined/specs/joined")
}

fn open(directory: &Path) -> lint::Opened {
    lint::open(directory, &Options::default())
        .expect("the fixture is readable")
        .unwrap_or_else(|| panic!("{} carries no spec.md", directory.display()))
}

fn defects_of(directory: &Path) -> Vec<Defect> {
    lint::lint_directory(directory, &Options::default())
        .expect("the fixture is readable")
        .unwrap_or_else(|| panic!("{} carries no spec.md", directory.display()))
}

fn shown(defects: &[Defect]) -> String {
    defects
        .iter()
        .map(Defect::to_string)
        .collect::<Vec<String>>()
        .join("; ")
}

/// Lint the joined tree with one file rewritten, so a break can be made without
/// committing a second broken tree per rule.
fn joined_with(rewrite: &[(&str, &str)]) -> Vec<Defect> {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let scratch = std::env::temp_dir().join(format!(
        "spec-lint-resolution-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&scratch);
    copy(&fixtures().join("joined"), &scratch);
    for (path, bytes) in rewrite {
        let full = scratch.join(path);
        std::fs::create_dir_all(full.parent().expect("a fixture path has a parent"))
            .expect("the scratch tree is writable");
        std::fs::write(&full, bytes).expect("the scratch tree is writable");
    }
    let defects = defects_of(&scratch.join("specs/joined"));
    let _ = std::fs::remove_dir_all(&scratch);
    defects
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("the scratch tree is writable");
    for entry in std::fs::read_dir(from).expect("the fixture tree is readable") {
        let entry = entry.expect("the fixture tree is readable");
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("the scratch tree is writable");
        }
    }
}

#[test]
fn the_joined_tree_resolves_with_no_defect() {
    let defects = defects_of(&joined());
    assert!(
        defects.is_empty(),
        "the clean joined tree reported {}",
        shown(&defects)
    );
    assert_eq!(Outcome::of(&defects), Outcome::Clean);
}

#[test]
fn a_spec_without_a_worklist_or_a_trace_is_joined_to_nothing_and_reports_nothing() {
    // The golden fixture is a bare identity directory: proposed, never derived,
    // no worklist and no trace. Absence is a lifecycle state, not a defect.
    let opened = open(&fixtures().join("golden"));
    assert!(opened.artifacts.worklist.is_none());
    assert!(opened.artifacts.trace.is_none());
    assert!(defects_of(&fixtures().join("golden")).is_empty());
}

#[test]
fn a_phantom_specs_pointer_fails_resolution_as_l13() {
    let defects = defects_of(&fixtures().join("must-fail/l13-phantom-pointer"));
    assert_eq!(defects.len(), 2, "{}", shown(&defects));
    assert!(defects.iter().all(|defect| defect.rule == RuleId::L13));
    assert!(defects
        .iter()
        .any(|defect| defect.message.contains("does not exist at this revision")));
    assert!(defects
        .iter()
        .any(|defect| defect.message.contains("offers no `#r9` anchor")));
    // The defect names the worklist, because that is the file that has to change.
    assert!(defects
        .iter()
        .all(|defect| defect.file.ends_with("l13-phantom-pointer.json")));
}

#[test]
fn an_orphan_trace_row_fails_resolution_as_l14() {
    let defects = defects_of(&fixtures().join("must-fail/l14-orphan-trace-row"));
    assert_eq!(defects.len(), 1, "{}", shown(&defects));
    assert_eq!(defects[0].rule, RuleId::L14);
    assert!(defects[0].message.contains("traces claim `9.9`"));
    assert!(defects[0].file.ends_with("trace.json"));
}

#[test]
fn a_trace_row_naming_an_unknown_task_or_acceptance_id_fails_resolution() {
    let defects = joined_with(&[(
        "specs/joined/trace.json",
        r#"{ "schemaVersion": 1, "spec": "specs/joined/spec.md", "rows": [
          { "seq": 1, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.1",
            "task": "the-first-task", "sitting": "joined/s1", "acceptance": ["the-pointers-resolve"] },
          { "seq": 2, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.2",
            "task": "the-second-task", "sitting": "joined/s1", "acceptance": ["no-such-criterion"] },
          { "seq": 3, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.1",
            "task": "no-such-task", "sitting": "joined/s1", "acceptance": ["the-pointers-resolve"] }
        ] }"#,
    )]);
    assert_eq!(defects.len(), 2, "{}", shown(&defects));
    assert!(defects.iter().all(|defect| defect.rule == RuleId::L14));
    assert!(defects
        .iter()
        .any(|defect| defect.message.contains("traces task `no-such-task`")));
    assert!(defects
        .iter()
        .any(|defect| defect.message.contains("acceptance id `no-such-criterion`")));
}

#[test]
fn a_release_row_before_its_sitting_row_fails_resolution() {
    let defects = joined_with(&[(
        "specs/joined/trace.json",
        r#"{ "schemaVersion": 1, "spec": "specs/joined/spec.md", "rows": [
          { "seq": 1, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.2",
            "task": "the-second-task", "sitting": "joined/s1", "acceptance": ["the-rows-resolve"] },
          { "seq": 2, "at": "2026-08-17T10:00:00Z", "kind": "release", "claim": "1.1",
            "task": "the-first-task",
            "merged": "0123456789abcdef0123456789abcdef01234567",
            "witness": "joined/summary/complete", "release": "joined-v0" }
        ] }"#,
    )]);
    assert!(defects
        .iter()
        .any(|defect| defect.rule == RuleId::L14
            && defect.message.contains("with no prior sitting row")));
}

#[test]
fn a_claim_traced_nowhere_and_staged_nowhere_fails_resolution() {
    // Both rows moved onto the claim the unauthored stage already excuses, so
    // 1.1 and 1.2 are the only claims left owing one. 2.1 sits under the
    // unauthored stage; U.1 is an unchanged line, bound to an oracle that
    // already passes, and belongs to no task at all.
    let defects = joined_with(&[(
        "specs/joined/trace.json",
        r#"{ "schemaVersion": 1, "spec": "specs/joined/spec.md", "rows": [
          { "seq": 1, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "2.1",
            "task": "the-first-task", "sitting": "joined/s1", "acceptance": ["the-pointers-resolve"] },
          { "seq": 2, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "2.1",
            "task": "the-second-task", "sitting": "joined/s1", "acceptance": ["the-rows-resolve"] }
        ] }"#,
    )]);
    assert_eq!(defects.len(), 2, "{}", shown(&defects));
    assert!(defects.iter().all(
        |defect| defect.rule == RuleId::L14 && defect.message.contains("is traced to no task")
    ));
    assert!(defects
        .iter()
        .any(|defect| defect.message.contains("claim `1.1`")));
    assert!(defects
        .iter()
        .any(|defect| defect.message.contains("claim `1.2`")));
}

#[test]
fn a_schema_invalid_trace_fails_resolution_against_its_committed_contract() {
    let defects = joined_with(&[(
        "specs/joined/trace.json",
        r#"{ "schemaVersion": 1, "spec": "specs/joined/spec.md", "rows": [
          { "seq": 1, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.1",
            "task": "the-first-task", "sitting": "joined/s1",
            "acceptance": ["the-pointers-resolve"], "stray": true },
          { "seq": 2, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.2",
            "task": "the-second-task", "sitting": "joined/s1", "acceptance": ["the-rows-resolve"] }
        ] }"#,
    )]);
    assert!(
        defects.iter().any(|defect| defect.rule == RuleId::L14
            && defect.message.contains("is invalid against")
            && defect.message.contains("stray")),
        "{}",
        shown(&defects)
    );
}

#[test]
fn a_trace_naming_another_identity_fails_resolution() {
    let defects = joined_with(&[(
        "specs/joined/trace.json",
        r#"{ "schemaVersion": 1, "spec": "specs/elsewhere/spec.md", "rows": [
          { "seq": 1, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.1",
            "task": "the-first-task", "sitting": "joined/s1", "acceptance": ["the-pointers-resolve"] },
          { "seq": 2, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.2",
            "task": "the-second-task", "sitting": "joined/s1", "acceptance": ["the-rows-resolve"] }
        ] }"#,
    )]);
    assert_eq!(defects.len(), 1, "{}", shown(&defects));
    assert_eq!(defects[0].rule, RuleId::L14);
    assert!(defects[0]
        .message
        .contains("it sits beside `specs/joined/spec.md`"));
}

#[test]
fn an_unresolvable_evidence_citation_fails_resolution_as_l13() {
    let defects = joined_with(&[(
        "specs/joined/trace.json",
        r#"{ "schemaVersion": 1, "spec": "specs/joined/spec.md", "rows": [
          { "seq": 1, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.1",
            "task": "the-first-task", "sitting": "joined/s1",
            "acceptance": ["the-pointers-resolve"],
            "evidence": ["specs/joined/evidence/absent.md"] },
          { "seq": 2, "at": "2026-08-17T09:00:00Z", "kind": "sitting", "claim": "1.2",
            "task": "the-second-task", "sitting": "joined/s1", "acceptance": ["the-rows-resolve"] }
        ] }"#,
    )]);
    assert_eq!(defects.len(), 1, "{}", shown(&defects));
    assert_eq!(defects[0].rule, RuleId::L13);
    assert!(defects[0].message.contains("cites evidence"));
}

#[test]
fn acceptance_domains_outside_the_declared_boundary_fail_resolution_as_l18() {
    let defects = defects_of(&fixtures().join("must-fail/l18-acceptance-domains"));
    assert_eq!(defects.len(), 2, "{}", shown(&defects));
    assert!(defects.iter().all(|defect| defect.rule == RuleId::L18));
    assert!(defects.iter().all(|defect| defect
        .message
        .contains("task `writes-outside-its-boundary`")));
    // The asserted file and the redirection target are both write positions:
    // neither can be true of a tree the lane never touched.
    assert!(defects.iter().any(|defect| defect
        .message
        .contains("acceptance `asserts-a-file-it-may-not-create` writes `flake.nix`")));
    assert!(defects.iter().any(|defect| defect
        .message
        .contains("acceptance `writes-a-report-it-may-not-own` writes `doc/report.md`")));
    // The defect names the worklist, because that is the file that has to change.
    assert!(defects
        .iter()
        .all(|defect| defect.file.ends_with("l18-acceptance-domains.json")));
}

#[test]
fn acceptance_domains_stay_silent_over_reads_and_over_an_undeclared_boundary() {
    // The same fixture carries the two silent cases: a task that reads a path
    // outside its boundary and writes only inside it, and a task that declares
    // no boundary at all. Both appear in the defect list above by absence.
    let reported = defects_of(&fixtures().join("must-fail/l18-acceptance-domains"));
    for task in ["stays-inside-its-boundary", "declares-no-boundary"] {
        assert!(
            !reported.iter().any(|defect| defect.message.contains(task)),
            "{task} reported: {}",
            shown(&reported)
        );
    }
}

#[test]
fn the_census_binds_every_claim_to_exactly_one_witnessed_oracle() {
    let opened = open(&joined());
    let (rows, defects) = census::census(
        &opened.document,
        &opened.context,
        opened.artifacts.worklist.as_ref(),
    );
    assert!(defects.is_empty(), "{}", shown(&defects));
    assert_eq!(
        census::render(&rows),
        std::fs::read_to_string(fixtures().join("joined.census.md"))
            .expect("the census golden is committed")
    );
    // Every one of the three admitted binding shapes is enumerated, and a gate
    // renders the argv the worklist witnesses for it.
    assert!(rows.iter().any(|row| row.binding == "check"));
    assert!(rows.iter().any(|row| row.binding == "HUMAN-ATTENDED"));
    assert!(rows
        .iter()
        .any(|row| row.binding == "gate" && row.oracle.contains("cargo test -p spec-lint")));
}

#[test]
fn the_census_reports_zero_two_and_unresolvable_bindings() {
    for (fixture, expected) in [
        ("must-fail/l9-binding-count", "binds no oracle"),
        ("must-fail/l9-doubly-bound-claim", "binds 2 oracles"),
    ] {
        let opened = open(&fixtures().join(fixture));
        let (_, defects) = census::census(
            &opened.document,
            &opened.context,
            opened.artifacts.worklist.as_ref(),
        );
        assert_eq!(defects.len(), 1, "{fixture}: {}", shown(&defects));
        assert_eq!(defects[0].rule, RuleId::L9);
        assert!(defects[0].message.contains(expected), "{fixture}");
    }

    // A gate id the governing worklist does not declare is the other half of
    // L9: a binding that names an oracle nobody can run. The joined tree read
    // against a worklist that declares `cargo-tests` and not `corpus-tests`
    // leaves both of its gate bindings unwitnessed.
    let opened = lint::open(
        &joined(),
        &Options {
            root: None,
            worklist: Some(fixtures().join(
                "must-fail/l14-orphan-trace-row/silent-factory-worklists/l14-orphan-trace-row.json",
            )),
        },
    )
    .expect("the fixture is readable")
    .expect("the fixture carries a spec.md");
    let (_, defects) = census::census(
        &opened.document,
        &opened.context,
        opened.artifacts.worklist.as_ref(),
    );
    assert_eq!(defects.len(), 2, "{}", shown(&defects));
    assert!(defects.iter().all(|defect| defect.rule == RuleId::L9
        && defect
            .message
            .contains("gate `corpus-tests` resolves to no gate id")));
}

#[test]
fn the_coverage_table_renders_the_join_byte_stable() {
    let golden = std::fs::read_to_string(fixtures().join("joined.coverage.md"))
        .expect("the coverage golden is committed");
    let mut rendered: Vec<String> = Vec::new();
    for _ in 0..3 {
        let opened = open(&joined());
        rendered.push(coverage::render(&coverage::coverage(
            &opened.document,
            opened.artifacts.trace.as_ref(),
        )));
    }
    assert!(
        rendered.iter().all(|run| *run == golden),
        "the coverage render drifted from tests/fixtures/joined.coverage.md:\n{}",
        rendered[0]
    );
}

#[test]
fn the_coverage_mode_writes_the_table_to_stdout_and_nothing_else() {
    let output = Command::new(env!("CARGO_BIN_EXE_spec-lint"))
        .arg("--coverage")
        .arg(joined())
        .output()
        .expect("the linter should launch");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).expect("the table is UTF-8"),
        std::fs::read_to_string(fixtures().join("joined.coverage.md"))
            .expect("the coverage golden is committed")
    );
}

#[test]
fn the_census_mode_prints_its_table_and_exits_on_the_defect_count() {
    let output = Command::new(env!("CARGO_BIN_EXE_spec-lint"))
        .arg("--census")
        .arg(fixtures().join("must-fail/l9-doubly-bound-claim"))
        .output()
        .expect("the linter should launch");
    assert_eq!(output.status.code(), Some(2));
    let table = String::from_utf8(output.stdout).expect("the table is UTF-8");
    assert!(table.starts_with("| claim | binding | oracle |\n|---|---|---|\n"));
    assert!(table.contains("| 1.1 | 2 bindings |"));
    let stderr = String::from_utf8(output.stderr).expect("defect lines are UTF-8");
    assert!(stderr.contains("L9: claim `1.1` binds 2 oracles"));
}

#[test]
fn the_worklist_flag_overrides_the_identity_convention() {
    let elsewhere = fixtures().join("joined/silent-factory-worklists/joined.json");
    let options = Options {
        root: None,
        worklist: Some(elsewhere),
    };
    // Pointed at the joined worklist, the l14 fixture's rows name a task no
    // longer declared — proof the flag, not the convention, chose the file.
    let defects =
        lint::lint_directory(&fixtures().join("must-fail/l14-orphan-trace-row"), &options)
            .expect("the fixture is readable")
            .expect("the fixture carries a spec.md");
    assert!(defects
        .iter()
        .any(|defect| defect.message.contains("traces task `the-task`")));
}

#[test]
fn every_resolution_rule_class_the_pass_evaluates_appears_in_the_corpus() {
    let bytes = std::fs::read_to_string(fixtures().join("must-fail/expected-defects.json"))
        .expect("the expected-defects map is committed");
    let expected: BTreeMap<String, usize> =
        serde_json::from_str(&bytes).expect("the map is a rule-id to count object");
    for rule in [RuleId::L13, RuleId::L14, RuleId::L18] {
        assert!(
            expected.contains_key(&rule.to_string()),
            "{rule} is evaluated by the resolution pass and has no must-fail fixture"
        );
    }
}
