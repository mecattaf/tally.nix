use std::path::Path;
use std::process::Command;

use serde_json::Value;

const EMPTY_CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/empty-config.json");

#[test]
fn lint_history_reports_each_commit_and_fails_a_poisoned_range() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path();
    git(repository, &["init", "-b", "main"]);
    git(repository, &["config", "user.name", "Fixture"]);
    git(
        repository,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(
        repository,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "feat(crates/tally): audit commit history",
        ],
    );
    let first = git(repository, &["rev-parse", "HEAD"]);
    git(
        repository,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "fix(crates/tally): reject poisoned trailers\n\nTally-Task: validator\npoison the trailer paragraph\nTally-Revision: sha256:abc",
        ],
    );

    let output = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["--config", EMPTY_CONFIG])
        .current_dir(repository)
        .args(["lint-history", "HEAD", "--scope", "crates/tally"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let verdicts = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(verdicts.len(), 2);
    assert_eq!(verdicts[0]["verdict"], "pass");
    assert_eq!(verdicts[1]["verdict"], "fail");
    assert_eq!(
        verdicts[1]["violations"][0]["rule"],
        "trailer-block-wellformed"
    );

    let valid = Command::new(env!("CARGO_BIN_EXE_tally"))
        .args(["--config", EMPTY_CONFIG])
        .current_dir(repository)
        .args(["lint-history", &first, "--scopes", "crates/tally"])
        .output()
        .unwrap();
    assert!(
        valid.status.success(),
        "{}",
        String::from_utf8_lossy(&valid.stderr)
    );
    let verdict: Value = serde_json::from_slice(&valid.stdout).unwrap();
    assert_eq!(verdict["commit"], first);
    assert_eq!(verdict["verdict"], "pass");
}

fn git(repository: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}
