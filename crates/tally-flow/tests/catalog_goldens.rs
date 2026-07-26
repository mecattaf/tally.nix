use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tally_flow::{load_catalog, resolve_members, SelectorOptions};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test/fixtures/flows")
        .join(name)
}

#[test]
fn selector_resolution_matches_the_helper_rendered_nix_catalog_goldens() {
    let catalog_path = std::env::var_os("TALLY_NIX_CATALOG_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| fixture("catalog-resolution.json"));
    let (catalog, catalog_hash) = load_catalog(&catalog_path).unwrap();
    let golden: Value =
        serde_json::from_slice(&fs::read(fixture("catalog-resolution.golden.json")).unwrap())
            .unwrap();

    assert!(catalog.members.iter().all(|member| member
        .classes
        .iter()
        .any(|class| class == "pooled-strongest")));
    assert_eq!(golden["pool"]["name"], "worker-gpu");
    assert_eq!(golden["pool"]["capacity"], 1);
    assert!(golden["capacityNote"]
        .as_str()
        .unwrap()
        .contains("membership only"));

    for case in golden["cases"].as_array().unwrap() {
        let selector = case["selector"].as_str().unwrap();
        let options: SelectorOptions = serde_json::from_value(case["options"].clone()).unwrap();
        let expected = case["expectedIds"].as_array().unwrap();

        for _ in 0..3 {
            let resolved = resolve_members(&catalog, &catalog_hash, selector, &options).unwrap();
            let ids = resolved
                .members
                .iter()
                .map(|member| Value::String(member.id.clone()))
                .collect::<Vec<_>>();
            assert_eq!(&ids, expected);
            assert!(resolved
                .members
                .iter()
                .all(|member| member.pools == ["worker-gpu"]));
        }
    }

    let selected = golden["cases"][1]["expectedIds"].as_array().unwrap().len();
    let capacity = golden["pool"]["capacity"].as_u64().unwrap() as usize;
    assert!(
        selected > capacity,
        "membership resolution must not truncate to execution capacity"
    );
}
