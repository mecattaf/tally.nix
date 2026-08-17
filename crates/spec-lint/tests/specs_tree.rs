//! The two tests that read the committed `specs/` tree: the §7 rule-index
//! parity `specs/README.md` names as its standing consumer, and the
//! accept/reject corpus at `specs/zeta/contracts/claim-line.fixtures.json`.
//!
//! The workspace derivation builds from a filtered source that does not carry
//! `./specs`, so both tests SKIP with a printed note when no ancestor of the
//! crate holds `specs/README.md`. Their bite is preserved where the full tree
//! exists: the `cargo-tests` gate runs in the complete worktree, and the flake
//! check passes `specs/` as an explicit source input.

// The skip note is harness output with no `Result` to return.
#![allow(clippy::disallowed_macros)]

use std::path::{Path, PathBuf};

use spec_lint::lint::{lint_text, Context};
use spec_lint::rules::{RuleId, CATALOG};

/// The nearest ancestor of this crate that carries `specs/README.md`.
fn specs_tree() -> Option<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|ancestor| ancestor.join("specs/README.md").is_file())
        .map(Path::to_path_buf)
}

fn skip(test: &str) {
    println!("{test}: skipped — no ancestor of this crate carries specs/README.md");
}

/// The `| rule | ... | severity |` rows of the §7 rule index.
fn rule_index(readme: &str) -> Vec<(String, String)> {
    readme
        .lines()
        .filter(|line| line.starts_with("| L"))
        .map(|line| {
            let cells: Vec<&str> = line
                .trim()
                .trim_matches('|')
                .split('|')
                .map(str::trim)
                .collect();
            assert_eq!(cells.len(), 4, "a rule-index row has four cells: {line}");
            (cells[0].to_owned(), cells[3].to_owned())
        })
        .collect()
}

#[test]
fn the_readme_rule_index_and_the_implemented_rule_set_stay_in_parity() {
    let Some(root) = specs_tree() else {
        skip("the_readme_rule_index_and_the_implemented_rule_set_stay_in_parity");
        return;
    };
    let readme =
        std::fs::read_to_string(root.join("specs/README.md")).expect("the README is readable");
    let index = rule_index(&readme);

    let catalogued: Vec<(String, String)> = CATALOG
        .iter()
        .map(|rule| (rule.id.to_string(), rule.severity.to_owned()))
        .collect();
    assert_eq!(
        index, catalogued,
        "specs/README.md §7 and crates/spec-lint's rule catalog have drifted"
    );
}

/// The synthetic spec one fixture line is read inside, and the line number the
/// fixture line lands on.
fn synthetic_spec(group: u64, claim: &str, vocabulary: &str) -> (String, usize) {
    let head = format!(
        "# fixtures — the claim-line accept/reject corpus\n\
         Status: proposed\n\
         Governs: silent-factory-worklists/fixtures.json\n\
         Consumers: the claim-line contract test\n\
         Supersedes: none\n\
         \n\
         ## Outcome\n\
         \n\
         One fixture line is read here. The spec around it is well formed. Only\n\
         the defects on the fixture line itself are read back.\n\
         \n\
         ## Vocabulary\n\
         \n\
         {vocabulary}\n\
         ## Rulings\n\
         \n\
         | id | decision | ruling |\n\
         |---|---|---|\n\
         | C1 | shape | the fixture line under test carries the claim grammar |\n\
         \n\
         ## Claims\n\
         \n\
         ### R{group} — the line under test\n\
         Why: the fixture corpus is the double pin between the README prose and this parser.\n"
    );
    let line = head.lines().count() + 1;
    let text = format!(
        "{head}{claim}\n\
         \n\
         ## Unchanged\n\
         \n\
         Omitted: nothing holds here.\n\
         \n\
         ## Unknowns\n\
         \n\
         Omitted: no doubt outstanding.\n\
         \n\
         ## Stages\n\
         \n\
         Omitted: nothing is built here.\n\
         \n\
         ## Forbidden\n\
         \n\
         F.1 Do not hand-edit this synthetic spec; the corpus is the source.\n"
    );
    (text, line)
}

/// The Vocabulary block of `specs/zeta/spec.md`, so a fixture line is read with
/// the vocabulary the corpus was written against.
fn zeta_vocabulary(root: &Path) -> String {
    let spec = std::fs::read_to_string(root.join("specs/zeta/spec.md"))
        .expect("the zeta spec is readable");
    let mut block = String::new();
    let mut inside = false;
    for line in spec.lines() {
        if line.starts_with("## ") {
            if inside {
                break;
            }
            inside = line == "## Vocabulary";
            continue;
        }
        if inside && !line.trim().is_empty() {
            block.push_str(line);
            block.push('\n');
        }
    }
    assert!(
        !block.is_empty(),
        "the zeta spec carries a Vocabulary block"
    );
    block.push('\n');
    block
}

#[test]
fn the_claim_line_fixture_corpus_is_honoured() {
    let Some(root) = specs_tree() else {
        skip("the_claim_line_fixture_corpus_is_honoured");
        return;
    };
    let corpus =
        std::fs::read_to_string(root.join("specs/zeta/contracts/claim-line.fixtures.json"))
            .expect("the claim-line corpus is readable");
    let corpus: serde_json::Value =
        serde_json::from_str(&corpus).expect("the claim-line corpus is JSON");
    let cases = corpus["cases"]
        .as_array()
        .expect("the corpus carries a cases array");
    assert!(!cases.is_empty());

    let vocabulary = zeta_vocabulary(&root);
    let context = Context {
        file: "specs/zeta/contracts/claim-line.fixtures.json".to_owned(),
        identity: "fixtures".to_owned(),
        directory: root.clone(),
        root: root.clone(),
    };

    for case in cases {
        let claim = case["line"].as_str().expect("a case carries its line");
        let group = case["group"].as_u64().expect("a case carries its group");
        let verdict = case["verdict"]
            .as_str()
            .expect("a case carries its verdict");
        let (text, number) = synthetic_spec(group, claim, &vocabulary);

        let reported: Vec<RuleId> = lint_text(&text, &context)
            .into_iter()
            .filter(|defect| defect.line == number)
            .map(|defect| defect.rule)
            .collect();

        match verdict {
            "accept" => assert!(
                reported.is_empty(),
                "the corpus accepts `{claim}`; the parser reported {reported:?}"
            ),
            "reject" => {
                let expected = case["rule"]
                    .as_str()
                    .and_then(RuleId::parse)
                    .expect("a rejected case names its rule");
                assert!(
                    reported.contains(&expected),
                    "the corpus rejects `{claim}` as {expected} ({}); the parser reported {reported:?}",
                    case["why"].as_str().unwrap_or_default()
                );
            }
            other => panic!("a case verdict is `accept` or `reject`, not `{other}`"),
        }
    }
}
