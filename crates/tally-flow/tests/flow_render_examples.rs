use std::fs;
use std::path::Path;

use tally_flow::render_script;

#[test]
fn every_shipped_flow_renders_as_embeddable_mermaid() {
    // The production CLI gives its Boa worker the same 4 MiB stack. The large
    // spec-build fixture is intentionally beyond Rust's default test-thread
    // stack even for the checker alone.
    std::thread::Builder::new()
        .name("flow-render-examples".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/flows");
            for name in [
                "academic-ocr.js",
                "agency-nightly.js",
                "domain-failure.js",
                "fleet-deploy.js",
                "monthly-review.js",
                "pooled-review.js",
                "spec-build.js",
                "worklist-fanout.js",
            ] {
                let path = examples.join(name);
                let source = fs::read_to_string(&path).unwrap();
                let rendered = render_script(&source, Some(&path))
                    .unwrap_or_else(|error| panic!("{name} did not render: {error}"));
                assert_eq!(rendered.lines().next(), Some("flowchart TD"), "{name}");
                assert!(
                    rendered.lines().any(|line| line.starts_with("    n")),
                    "{name} rendered no node call sites"
                );
                assert!(!rendered.contains("```"), "{name} included a fence");
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
