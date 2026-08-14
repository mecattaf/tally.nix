use std::path::Path;
use std::thread;

use spec_build_driver::flow_args::{
    flow_args_schema, generated_schema_block, rendered_flow_args_schema_property,
};
use tally_flow::{check_script, CheckOptions};

const SOURCE: &str = include_str!("../../../examples/flows/spec-build.js");

#[test]
fn spec_build_args_schema_is_the_exact_rust_generated_golden() {
    let expected_block = format!("{}\n", rendered_flow_args_schema_property());
    assert_eq!(
        generated_schema_block(SOURCE),
        Some(expected_block.as_str()),
        "spec-build.js has a stale argsSchema; run \
         `cargo run -p spec-build-driver --example generate-flow-args-schema`"
    );

    let checked = thread::Builder::new()
        .name("spec-build-args-schema-golden".to_owned())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            check_script(
                SOURCE,
                Some(Path::new("examples/flows/spec-build.js")),
                CheckOptions::default(),
            )
            .expect("the Rust-generated spec-build flow must satisfy the dialect")
        })
        .expect("schema golden worker must start")
        .join()
        .expect("schema golden worker must stop cleanly");

    assert_eq!(checked.meta.args_schema, flow_args_schema());
}
