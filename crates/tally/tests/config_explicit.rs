use std::fs;
use std::path::Path;

fn rust_sources_below(directory: &Path, sources: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_sources_below(&path, sources);
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
            sources.push(path);
        }
    }
}

fn rust_test_sources() -> Vec<std::path::PathBuf> {
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut sources = Vec::new();
    rust_sources_below(&tests, &mut sources);
    sources.sort();
    sources
}

#[test]
fn every_direct_tally_spawn_names_its_config() {
    // Assemble the needle so this source-enumeration test does not enumerate
    // its own search string as a subprocess constructor.
    let needle = ["Command::new(env!(\"", "CARGO_BIN_EXE_", "tally", "\"))"].concat();
    let mut spawns = 0usize;
    for path in rust_test_sources() {
        let source = fs::read_to_string(&path).unwrap();
        for (offset, _) in source.match_indices(&needle) {
            spawns += 1;
            let end = source.len().min(offset + needle.len() + 384);
            let constructor = &source[offset..end];
            assert!(
                constructor.contains("\"--config\""),
                "{} has a direct tally subprocess without an explicit --config near byte {}",
                path.display(),
                offset
            );
        }
    }
    assert!(
        spawns > 0,
        "the tally subprocess census unexpectedly found no sites"
    );
}

#[test]
fn every_tally_binary_reference_is_bound_to_an_explicit_config() {
    // This wider census includes programs handed to the executor or daemon,
    // where the eventual process construction lives outside this crate.
    let needle = ["env!(\"", "CARGO_BIN_EXE_", "tally", "\")"].concat();
    let mut references = 0usize;
    for path in rust_test_sources() {
        let source = fs::read_to_string(&path).unwrap();
        for (offset, _) in source.match_indices(&needle) {
            references += 1;
            let start = offset.saturating_sub(512);
            let end = source.len().min(offset + needle.len() + 512);
            assert!(
                source[start..end].contains("--config"),
                "{} references the tally binary without an explicit --config near byte {}",
                path.display(),
                offset
            );
        }
    }
    assert!(
        references > 0,
        "the tally binary census unexpectedly found no references"
    );
}
