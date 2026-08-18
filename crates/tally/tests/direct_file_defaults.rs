//! Issue #416: the direct-file verb family resolves an omitted `--data-dir`
//! through `TALLY_DATA_DIR` before falling back to the XDG default.
//!
//! Precedence, proven at the `default_data_dir()` seam: an explicit
//! `--data-dir` flag wins, then `TALLY_DATA_DIR` taken verbatim, then
//! `$XDG_DATA_HOME/tally`, else `~/.local/share/tally`. The failure this
//! closes is the silent no-op: `reader-state archive` against the wrong
//! store creates a brand-new store there, prints an affirmative record,
//! exits 0, and changes nothing any `query` command shows.
//!
//! Every case runs the real binary as a subprocess, so the environment
//! manipulation cannot leak into a sibling test.

use std::path::Path;
use std::process::Output;

use tokio::process::Command;

#[path = "support/isolated_host.rs"]
mod isolated_host;

use isolated_host::{Isolated, IsolatedHost};

const FLOW_RUN: &str = "00000000-0000-4000-8000-000000000abc";
const EMPTY_CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/empty-config.json");
const VALID_LEDGER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../test/fixtures/ledger/valid.jsonl"
);

async fn tally_invocation(home: &Path, data_dir: Option<&Path>, args: &[&str]) -> Output {
    invocation(home, None, data_dir, args).await
}

async fn invocation(
    home: &Path,
    xdg_data_home: Option<&Path>,
    data_dir: Option<&Path>,
    args: &[&str],
) -> Output {
    // Every case here is *about* the host locations, so the guard binds them
    // first and the case then aims the ones it is proving. What the guard
    // removes is the possibility of a variable this file never mentions --
    // `XDG_STATE_HOME`, the socket -- resolving to the operator's.
    let host = IsolatedHost::new();
    let mut command = Command::new(env!("CARGO_BIN_EXE_tally"));
    command
        .isolated(&host)
        .args(["--config", EMPTY_CONFIG])
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME");
    match xdg_data_home {
        Some(xdg) => command.env("XDG_DATA_HOME", xdg),
        None => command.env_remove("XDG_DATA_HOME"),
    };
    match data_dir {
        Some(data_dir) => command.env("TALLY_DATA_DIR", data_dir),
        None => command.env_remove("TALLY_DATA_DIR"),
    };
    command.output().await.unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `TALLY_DATA_DIR` aims the write verbs at the deployment's store: the
/// record lands there and nowhere else, and the unchanged affirmative output
/// is now a claim about the right store.
#[tokio::test]
async fn tally_data_dir_beats_the_xdg_default() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("fakehome");
    let deploy = temp.path().join("deploy");

    let output = tally_invocation(
        &home,
        Some(&deploy),
        &[
            "reader-state",
            "archive",
            FLOW_RUN,
            "--tag",
            "flaky-fixture",
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stdout(&output).contains("\"archived\":true"),
        "{}",
        stdout(&output)
    );
    assert!(
        deploy.join("reader-state.jsonl").is_file(),
        "the store must land where TALLY_DATA_DIR points"
    );
    assert!(
        !home.join(".local/share/tally/reader-state.jsonl").exists(),
        "nothing may be created in the XDG default store while the variable is set"
    );
}

/// The explicit flag still wins: the variable is the default's business, not
/// the caller's override.
#[tokio::test]
async fn an_explicit_data_dir_flag_beats_tally_data_dir() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("fakehome");
    let deploy = temp.path().join("deploy");
    let explicit = temp.path().join("explicit");

    let output = tally_invocation(
        &home,
        Some(&deploy),
        &[
            "reader-state",
            "archive",
            FLOW_RUN,
            "--data-dir",
            explicit.to_str().unwrap(),
        ],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        explicit.join("reader-state.jsonl").is_file(),
        "the flag names the store"
    );
    assert!(
        !deploy.join("reader-state.jsonl").exists(),
        "the variable must not win over an explicit flag"
    );
    assert!(
        !home.join(".local/share/tally/reader-state.jsonl").exists(),
        "and neither may the XDG default"
    );
}

/// Unset or empty, the variable changes nothing: local use keeps resolving
/// `$XDG_DATA_HOME/tally`, else `~/.local/share/tally`, exactly as before.
#[tokio::test]
async fn without_tally_data_dir_the_xdg_default_is_unchanged() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("fakehome");

    let output = tally_invocation(&home, None, &["reader-state", "archive", FLOW_RUN]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        home.join(".local/share/tally/reader-state.jsonl").is_file(),
        "the HOME fallback is untouched by this change"
    );

    // An empty variable is the same fact as an unset one: a bare
    // `TALLY_DATA_DIR=` must not resolve to the current directory.
    let fresh = temp.path().join("fakehome2");
    let output = tally_invocation(
        &fresh,
        Some(Path::new("")),
        &["reader-state", "archive", FLOW_RUN],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        fresh
            .join(".local/share/tally/reader-state.jsonl")
            .is_file(),
        "an empty TALLY_DATA_DIR falls through to the XDG default"
    );
    assert!(
        !Path::new("reader-state.jsonl").exists(),
        "an empty variable must not aim the verb at the current directory"
    );
}

/// The read half of the family: unchanged for local use, and aimed by the
/// variable the same way.
#[tokio::test]
async fn read_verbs_keep_working_locally_and_follow_the_variable() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("fakehome");
    let deploy = temp.path().join("deploy");

    // `reader-state show` reads what `archive` wrote through the same
    // default: the family stays aimed at one store for local use.
    let output = tally_invocation(&home, None, &["reader-state", "archive", FLOW_RUN]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let output = tally_invocation(&home, None, &["reader-state", "show", FLOW_RUN]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stdout(&output).contains("\"archived\":true"),
        "{}",
        stdout(&output)
    );

    // `witness verify` against an empty local store still reports a clean,
    // empty chain.
    let output = tally_invocation(&home, None, &["witness", "verify"]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        stdout(&output).contains("verdict chain: ok (0 records"),
        "{}",
        stdout(&output)
    );

    // Seed the deployment store with the known-good ledger fixture and point
    // the variable at it: the verification must read that ledger, not the
    // empty default store.
    std::fs::create_dir_all(&deploy).unwrap();
    std::fs::copy(VALID_LEDGER, deploy.join("witness.jsonl")).unwrap();
    let output = tally_invocation(&home, Some(&deploy), &["witness", "verify"]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let text = stdout(&output);
    assert!(
        text.contains("verdict chain: ok") && !text.contains("verdict chain: ok (0 records"),
        "the seeded ledger, not the empty default store, must be the one verified: {text}"
    );
}

/// The middle tier of the precedence, proven against a *set* `XDG_DATA_HOME`
/// rather than only against the `HOME` fallback: with both present the
/// variable wins, and with the variable gone the XDG value is still what
/// resolves. That is the whole order — flag, variable, XDG default — bound in
/// one place.
#[tokio::test]
async fn tally_data_dir_beats_a_set_xdg_data_home_and_yields_to_none() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("fakehome");
    let xdg = temp.path().join("xdg");
    let deploy = temp.path().join("deploy");

    let output = invocation(
        &home,
        Some(&xdg),
        Some(&deploy),
        &["reader-state", "archive", FLOW_RUN],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        deploy.join("reader-state.jsonl").is_file(),
        "the variable outranks a set XDG_DATA_HOME"
    );
    assert!(
        !xdg.join("tally/reader-state.jsonl").exists(),
        "and nothing may be written under XDG_DATA_HOME while it is set"
    );

    // Same environment minus the variable: `$XDG_DATA_HOME/tally` is what the
    // verb resolves, exactly as it did before the variable existed.
    let output = invocation(
        &home,
        Some(&xdg),
        None,
        &["reader-state", "archive", FLOW_RUN],
    )
    .await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        xdg.join("tally/reader-state.jsonl").is_file(),
        "without the variable the XDG default is unchanged"
    );
    assert!(
        !home.join(".local/share/tally/reader-state.jsonl").exists(),
        "and the HOME fallback stays behind XDG_DATA_HOME"
    );
}

/// The variable is taken verbatim as the directory, not searched: pointed at
/// something that cannot hold the store, the write verb fails naming that
/// path. It must not fall back to the XDG default, because a fallback would
/// restore the exact failure #416 closes — an affirmative success line about
/// a store somewhere else.
#[tokio::test]
async fn an_unusable_tally_data_dir_fails_loudly_instead_of_falling_back() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("fakehome");
    let occupied = temp.path().join("not-a-directory");
    std::fs::write(&occupied, b"").unwrap();

    let output = tally_invocation(
        &home,
        Some(&occupied),
        &["reader-state", "archive", FLOW_RUN],
    )
    .await;
    assert_ne!(output.status.code(), Some(0), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains(occupied.to_str().unwrap()),
        "the refusal must name the path it was aimed at: {stderr}"
    );
    assert!(
        stdout(&output).is_empty(),
        "nothing may claim a record was written: {}",
        stdout(&output)
    );
    assert!(
        !home.join(".local/share/tally/reader-state.jsonl").exists(),
        "an unusable variable must not silently resolve to the user default"
    );
}
