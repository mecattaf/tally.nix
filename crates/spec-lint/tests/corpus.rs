//! The bite proof. A linter never shown to bite is the `--list-only` flake
//! attribute reborn: the golden fixture proves the rules stay silent over a
//! clean spec, and the must-fail corpus proves each rule class fires, in the
//! exact counts `expected-defects.json` records.
//!
//! Every fixture here is crate-local, so these tests run wherever the crate
//! builds — including a sandbox whose source carries no `specs/` tree.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use spec_lint::defect::{Defect, Outcome, Severity};
use spec_lint::lint::{self, Options};
use spec_lint::rules::RuleId;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn corpus_directories() -> Vec<PathBuf> {
    let mut directories: Vec<PathBuf> = std::fs::read_dir(fixtures().join("must-fail"))
        .expect("the must-fail corpus is committed")
        .map(|entry| entry.expect("the corpus is readable").path())
        .filter(|path| path.is_dir())
        .collect();
    directories.sort();
    directories
}

fn defects_of(directory: &Path) -> Vec<Defect> {
    lint::lint_directory(directory, &Options::default())
        .expect("the fixture is readable")
        .unwrap_or_else(|| panic!("{} carries no spec.md", directory.display()))
}

fn expected_map() -> BTreeMap<String, usize> {
    let bytes = std::fs::read_to_string(fixtures().join("must-fail/expected-defects.json"))
        .expect("the expected-defects map is committed");
    serde_json::from_str(&bytes).expect("the expected-defects map is a rule-id to count object")
}

#[test]
fn the_golden_fixture_is_clean() {
    let defects = defects_of(&fixtures().join("golden"));
    assert!(
        defects.is_empty(),
        "the golden fixture reported {} defect(s): {}",
        defects.len(),
        defects
            .iter()
            .map(Defect::to_string)
            .collect::<Vec<String>>()
            .join("; ")
    );
    assert_eq!(Outcome::of(&defects), Outcome::Clean);
    assert_eq!(Outcome::of(&defects).exit_code(), 0);
}

#[test]
fn the_must_fail_corpus_reproduces_the_expected_defect_map() {
    let mut observed: BTreeMap<String, usize> = BTreeMap::new();
    let mut all: Vec<Defect> = Vec::new();
    for directory in corpus_directories() {
        for defect in defects_of(&directory) {
            *observed.entry(defect.rule.to_string()).or_default() += 1;
            all.push(defect);
        }
    }

    let expected = expected_map();
    assert_eq!(
        observed,
        expected,
        "the corpus produced {}",
        all.iter()
            .map(Defect::to_string)
            .collect::<Vec<String>>()
            .join("\n")
    );
    assert_eq!(Outcome::of(&all), Outcome::Blocking);
}

#[test]
fn every_corpus_directory_fires_the_rule_its_name_carries() {
    for directory in corpus_directories() {
        let name = directory
            .file_name()
            .expect("a corpus directory has a name")
            .to_string_lossy()
            .into_owned();
        let prefix = name.split('-').next().expect("a name has a prefix");
        let expected = RuleId::parse(&prefix.to_uppercase())
            .unwrap_or_else(|| panic!("{name} does not open with a rule id"));

        let defects = defects_of(&directory);
        assert!(
            defects.iter().all(|defect| defect.rule == expected),
            "{name} reported a rule other than {expected}: {}",
            defects
                .iter()
                .map(Defect::to_string)
                .collect::<Vec<String>>()
                .join("; ")
        );
        assert!(!defects.is_empty(), "{name} reported nothing");
    }
}

#[test]
fn a_warning_only_fixture_stops_short_of_blocking() {
    for name in ["l11-vocabulary-unused", "l15-empty-rulings"] {
        let defects = defects_of(&fixtures().join("must-fail").join(name));
        assert!(
            defects
                .iter()
                .all(|defect| defect.severity == Severity::Warning),
            "{name} should report warnings only"
        );
        assert_eq!(Outcome::of(&defects), Outcome::Warnings);
    }
}

#[test]
fn a_directory_without_a_spec_is_skipped_silently() {
    let skipped = lint::lint_directory(&fixtures().join("evidence-only"), &Options::default())
        .expect("the fixture directory is readable");
    assert!(skipped.is_none());
}

#[test]
fn the_binary_maps_each_outcome_onto_its_exit_code() {
    let cases: [(&str, i32); 4] = [
        ("golden", 0),
        ("must-fail/l15-empty-rulings", 1),
        ("must-fail/l9-binding-count", 2),
        ("evidence-only", 0),
    ];
    for (fixture, code) in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_spec-lint"))
            .arg(fixtures().join(fixture))
            .output()
            .expect("the linter should launch");
        assert_eq!(
            output.status.code(),
            Some(code),
            "{fixture} exited {:?}; stderr: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "{fixture} wrote to stdout; defect lines belong on stderr"
        );
        let stderr = String::from_utf8(output.stderr).expect("defect lines are UTF-8");
        if code == 0 {
            assert!(stderr.is_empty(), "{fixture} should be silent");
        } else {
            for line in stderr.lines() {
                let mut fields = line.splitn(4, ": ");
                let anchor = fields.next().unwrap_or_default();
                let rule = fields.next().unwrap_or_default();
                let message = fields.next().unwrap_or_default();
                assert!(
                    anchor.contains("/spec.md:"),
                    "a defect line opens `<file>:<line>`: {line}"
                );
                assert!(
                    RuleId::parse(rule).is_some(),
                    "a defect line names a catalogued rule: {line}"
                );
                assert!(
                    !message.is_empty(),
                    "a defect line carries a message: {line}"
                );
            }
        }
    }
}

#[test]
fn every_rule_class_the_core_pass_evaluates_appears_in_the_corpus() {
    let corpus: Vec<String> = expected_map().into_keys().collect();
    for rule in spec_lint::rules::CATALOG {
        if rule.stage != spec_lint::rules::Stage::Core {
            continue;
        }
        assert!(
            corpus.contains(&rule.id.to_string()),
            "{} is evaluated by the check pass and has no must-fail fixture",
            rule.id
        );
    }
}
