//! The test-isolation guard, proven (`EPSILON-EXTENSION.md` ext2, the F25
//! class).
//!
//! Three claims, in the order they matter:
//!
//! 1. The danger is real — a `tally` spawned with a home it inherited writes
//!    into that home. Witnessed live on 2026-08-18 against the operator's own
//!    daemon and registry (`specs/eta/evidence/run-log.md`).
//! 2. [`IsolatedHost`] closes it — the same spawn, bound to a private host,
//!    leaves the inherited home untouched and lands everything under the
//!    private root instead.
//! 3. Nothing escapes it — every `tally` subprocess this crate's tests
//!    construct is bound to the guard, censused from the sources the same way
//!    `config_explicit.rs` censuses the explicit `--config`.
//!
//! Claim 1's probe is the one deliberately unbound spawn in the tree. It is
//! marked `ISOLATION_PROBE` and the census both exempts that marker and
//! requires it to appear exactly once, so the exemption cannot spread.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/isolated_host.rs"]
mod isolated_host;

use isolated_host::{Isolated, IsolatedHost, BOUND_VARIABLES, SCRUBBED_VARIABLES};

const EMPTY_CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/empty-config.json");
const FLOW_RUN: &str = "00000000-0000-4000-8000-00000000f25e";

/// The XDG default store the direct-file verbs create under a home when
/// nothing redirects them (`cli/exit.rs::default_data_dir`). Writing it is the
/// cheapest honest proof that a spawn reached a home.
const XDG_DEFAULT_STORE: &str = ".local/share/tally/reader-state.jsonl";

fn rust_sources_below(directory: &Path, sources: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources_below(&path, sources);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            sources.push(path);
        }
    }
}

fn rust_test_sources() -> Vec<PathBuf> {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut sources = Vec::new();
    rust_sources_below(&tests, &mut sources);
    sources.sort();
    sources
}

/// A verb that resolves a home-derived path and writes to it, so "did this
/// spawn reach that home" is answered by the filesystem and not by parsing.
///
/// The caller supplies the constructor and its `--config`, so both censuses
/// still read a complete spawn site at the call.
fn archive_into_the_xdg_default(command: &mut Command) -> std::process::Output {
    command
        .args([
            "reader-state",
            "archive",
            FLOW_RUN,
            "--tag",
            "host-isolation-probe",
        ])
        // The two redirects that would answer before the home does. Removing
        // them is what makes the home the thing under test.
        .env_remove("XDG_DATA_HOME")
        .env_remove("TALLY_DATA_DIR")
        .output()
        .unwrap()
}

/// The class, reproduced and then closed, in one test so the two halves can
/// never drift into proving different things.
///
/// The "host" here is a directory standing in for the operator's: the probe
/// hands it over explicitly rather than reading the real `HOME`, because a
/// test that proves the danger by writing into the operator's actual home has
/// committed the very defect it is describing.
#[test]
fn isolation_guard_bites_a_tally_spawned_with_the_host_home() {
    let temporary = tempfile::tempdir().unwrap();
    let host_home = temporary.path().join("operator-home");
    fs::create_dir_all(&host_home).unwrap();

    // ISOLATION_PROBE: the one unbound spawn in the tree. It inherits the home
    // it is handed exactly as an unguarded spawn inherits the operator's.
    let unbound = archive_into_the_xdg_default(
        Command::new(env!("CARGO_BIN_EXE_tally"))
            .args(["--config", EMPTY_CONFIG])
            .env("HOME", &host_home),
    );
    assert!(
        unbound.status.success(),
        "the probe verb must succeed for its write to mean anything: {}",
        String::from_utf8_lossy(&unbound.stderr)
    );
    assert!(
        host_home.join(XDG_DEFAULT_STORE).is_file(),
        "an unbound spawn must reach the home it inherited, or this test is \
         proving nothing about the guard that stops it"
    );

    // The same spawn, bound. The inherited home is still offered — the guard
    // is what overrides it — and nothing lands there.
    let guarded_home = temporary.path().join("operator-home-guarded");
    fs::create_dir_all(&guarded_home).unwrap();
    let host = IsolatedHost::new();
    let bound = archive_into_the_xdg_default(
        Command::new(env!("CARGO_BIN_EXE_tally"))
            .args(["--config", EMPTY_CONFIG])
            .env("HOME", &guarded_home)
            .isolated(&host),
    );
    assert!(
        bound.status.success(),
        "{}",
        String::from_utf8_lossy(&bound.stderr)
    );
    assert!(
        !guarded_home.join(".local").exists(),
        "a bound spawn wrote into the home it was handed: {}",
        guarded_home.display()
    );
    assert!(
        host.home().join(XDG_DEFAULT_STORE).is_file(),
        "the write has to land somewhere, and the private home is where"
    );
}

/// The guard binds every host location, not just the famous one.
///
/// `HOME` is the variable everyone remembers; `XDG_STATE_HOME` is the one that
/// reaches past a rebound home straight back to the operator's state, and
/// `TALLY_SOCKET`/`XDG_RUNTIME_DIR` are the pair that reached the operator's
/// live daemon on 2026-08-18.
#[test]
fn isolation_guard_binds_every_host_location_a_spawned_tally_resolves() {
    let host = IsolatedHost::new();
    let bindings = host.bindings();
    assert_eq!(bindings.len(), BOUND_VARIABLES.len());
    for (variable, value) in &bindings {
        assert!(
            value.starts_with(host.root()),
            "{variable} is bound outside the private root: {}",
            value.display()
        );
        if let Some(ambient) = std::env::var_os(variable) {
            assert_ne!(
                value.as_os_str(),
                ambient,
                "{variable} binds to the value this process inherited"
            );
        }
    }
    // Every directory exists before the first spawn: a runtime directory that
    // is merely absent produces a failure that reads as a product bug.
    for (variable, value) in &bindings {
        let directory = if value.extension().and_then(|value| value.to_str()) == Some("sock") {
            value.parent().unwrap()
        } else {
            value.as_path()
        };
        assert!(
            directory.is_dir(),
            "{variable} names a directory the guard never created: {}",
            directory.display()
        );
    }
    assert_eq!(host.socket().parent().unwrap(), host.runtime_dir());

    // The scrubbed set is disjoint from the bound set by construction: a
    // variable cannot both be given a private value and be removed.
    for variable in SCRUBBED_VARIABLES {
        assert!(
            host.binding(variable).is_none(),
            "{variable} is both bound and scrubbed"
        );
    }
}

/// The census. A spawn site that forgets the guard is a suite failure, which
/// is what makes the guard a guard rather than a convention.
///
/// Two exits are accepted, both of them isolation and neither of them an
/// oversight: `env_clear()`, which inherits nothing at all, and the single
/// marked probe above.
#[test]
fn every_spawned_tally_is_bound_to_the_isolation_guard() {
    // Assembled, so this census does not enumerate its own search string, and
    // so the exemption marker is not spelled out in the code that honours it.
    let needle = ["Command::new(env!(\"", "CARGO_BIN_EXE_", "tally", "\"))"].concat();
    let marker = ["ISOLATION", "_PROBE"].concat();
    let marked_site = format!("// {marker}");
    let mut spawns = 0usize;
    let mut probes = 0usize;
    for path in rust_test_sources() {
        let source = fs::read_to_string(&path).unwrap();
        probes += source.matches(marked_site.as_str()).count();
        for (offset, _) in source.match_indices(&needle) {
            spawns += 1;
            // The window reaches back as well as forward: a site whose builder
            // is handed to a helper binds the guard before the constructor.
            let start = offset.saturating_sub(256);
            let end = source.len().min(offset + needle.len() + 512);
            let site = &source[start..end];
            assert!(
                site.contains(".isolated(")
                    || site.contains("env_clear()")
                    || site.contains(marker.as_str()),
                "{} spawns tally without the isolation guard near byte {}; bind it \
                 with `.isolated(&host)` (support/isolated_host.rs)",
                path.display(),
                offset
            );
        }
    }
    assert!(
        spawns > 0,
        "the tally subprocess census unexpectedly found no sites"
    );
    // One marker, in the probe that needs it, and nowhere else. Without this
    // the exemption is a comment anyone can paste.
    assert_eq!(
        probes, 1,
        "the unbound-spawn exemption belongs to one probe; a second marked \
         site is a hole in the guard, not an exception to it"
    );
}
