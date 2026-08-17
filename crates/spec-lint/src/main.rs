use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use spec_lint::defect::{Defect, Outcome};
use spec_lint::lint;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Mode {
    /// Lint each directory's `spec.md` against the rule set.
    #[value(name = "check")]
    Check,
}

#[derive(Debug, Parser)]
#[command(
    name = "spec-lint",
    about = "Lint specs/<identity>/ directories against the specs/README.md rule set"
)]
struct Cli {
    /// The lint mode.
    #[arg(long, value_enum, default_value = "check")]
    mode: Mode,

    /// The working-tree root that BELIEVE paths and backticked paths resolve
    /// against. Defaults to the parent of `specs/` for a directory under one,
    /// and to the directory itself otherwise.
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// One or more specs/<identity>/ directories. A directory without a
    /// spec.md is skipped silently.
    #[arg(value_name = "DIR", required = true)]
    directories: Vec<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let Mode::Check = cli.mode;

    let mut defects: Vec<Defect> = Vec::new();
    for directory in &cli.directories {
        match lint::lint_directory(directory, cli.root.as_deref()) {
            Ok(Some(found)) => defects.extend(found),
            Ok(None) => {}
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "spec-lint: {error:#}");
                return ExitCode::from(Outcome::Blocking.exit_code());
            }
        }
    }

    let mut stderr = io::stderr().lock();
    for defect in &defects {
        if writeln!(stderr, "{defect}").is_err() {
            return ExitCode::from(Outcome::Blocking.exit_code());
        }
    }

    ExitCode::from(Outcome::of(&defects).exit_code())
}
