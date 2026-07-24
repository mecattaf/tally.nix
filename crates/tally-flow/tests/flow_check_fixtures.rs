use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use tally_flow::{check_script, load_catalog, CheckOptions};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test/fixtures/flows")
        .join(name)
}

fn check_failure(name: &str) -> tally_flow::FlowError {
    let path = fixture(name);
    let source = fs::read_to_string(&path).unwrap();
    check_script(&source, Some(&path), CheckOptions::default()).unwrap_err()
}

#[test]
fn nix_eval_time_fixture_contract_accepts_valid_and_rejects_each_class() {
    let path = fixture("valid.js");
    let source = fs::read_to_string(&path).unwrap();
    let (catalog, catalog_hash) = load_catalog(&fixture("catalog.json")).unwrap();
    let args = json!({"task": "ship"});
    let checked = check_script(
        &source,
        Some(&path),
        CheckOptions {
            args: Some(&args),
            catalog: Some(&catalog),
            catalog_hash: Some(&catalog_hash),
        },
    )
    .unwrap();
    assert_eq!(checked.meta.name, "fixture-valid");

    for (name, code) in [
        ("nonliteral-meta.js", "meta-nonliteral"),
        ("banned-global.js", "determinism-violation"),
        ("undeclared-pool.js", "undeclared-pool"),
        ("bad-args-schema.js", "args-schema-invalid"),
    ] {
        let error = check_failure(name);
        assert_eq!(error.code, code, "wrong error for {name}: {error}");
        assert!(error.location.is_some(), "missing location for {name}");
    }
}

#[test]
fn args_fixture_mismatch_is_typed() {
    let path = fixture("valid.js");
    let source = fs::read_to_string(&path).unwrap();
    let error = check_script(
        &source,
        Some(&path),
        CheckOptions {
            args: Some(&json!({"task": 7})),
            ..CheckOptions::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "args-schema-mismatch");
    assert!(error.location.is_some());
}
