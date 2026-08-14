use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use spec_build_driver::flow_args::replace_generated_flow_args_schema;

fn main() -> ExitCode {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    let (check, explicit_path) = match arguments.as_slice() {
        [] => (false, None),
        [flag] if flag == "--check" => (true, None),
        [path] => (false, Some(path.clone())),
        [flag, path] if flag == "--check" => (true, Some(path.clone())),
        _ => {
            eprintln!("usage: generate-flow-args-schema [--check] [FLOW_PATH]");
            return ExitCode::FAILURE;
        }
    };
    let path = explicit_path.map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/flows/spec-build.js"),
        PathBuf::from,
    );
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("cannot read {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let generated = match replace_generated_flow_args_schema(&source) {
        Ok(generated) => generated,
        Err(error) => {
            eprintln!("cannot generate {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    if check {
        if generated == source {
            return ExitCode::SUCCESS;
        }
        eprintln!(
            "{} is stale; run `cargo run -p spec-build-driver --example generate-flow-args-schema`",
            path.display()
        );
        return ExitCode::FAILURE;
    }
    if generated != source {
        if let Err(error) = fs::write(&path, generated) {
            eprintln!("cannot write {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}
