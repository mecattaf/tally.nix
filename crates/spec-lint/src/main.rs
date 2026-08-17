use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use spec_lint::defect::{Defect, Outcome};
use spec_lint::lint::{self, Options};
use spec_lint::{census, coverage};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Mode {
    /// Lint each directory's `spec.md` against the rule set, then resolve it
    /// against the governing worklist and `trace.json`.
    #[value(name = "check")]
    Check,
    /// Enumerate every claim's oracle binding.
    #[value(name = "census")]
    Census,
    /// Render the claim ↔ task ↔ acceptance-id ↔ evidence join.
    #[value(name = "coverage")]
    Coverage,
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

    /// Shorthand for `--mode census`.
    #[arg(long, conflicts_with = "coverage")]
    census: bool,

    /// Shorthand for `--mode coverage`.
    #[arg(long)]
    coverage: bool,

    /// The working-tree root that BELIEVE paths, backticked paths, and
    /// `specs/**` worklist pointers resolve against. Defaults to the parent of
    /// `specs/` for a directory under one, and to the directory itself
    /// otherwise.
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,

    /// The governing worklist. Defaults to
    /// `<root>/silent-factory-worklists/<identity>.json` for each directory,
    /// which is the only form that stays right for more than one directory.
    #[arg(long, value_name = "FILE")]
    worklist: Option<PathBuf>,

    /// One or more specs/<identity>/ directories. A directory without a
    /// spec.md is skipped silently.
    #[arg(value_name = "DIR", required = true)]
    directories: Vec<PathBuf>,
}

impl Cli {
    fn mode(&self) -> Mode {
        match (self.census, self.coverage) {
            (true, _) => Mode::Census,
            (_, true) => Mode::Coverage,
            _ => self.mode,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let options = Options {
        root: cli.root.clone(),
        worklist: cli.worklist.clone(),
    };

    let mut defects: Vec<Defect> = Vec::new();
    let mut rendered = String::new();
    for directory in &cli.directories {
        // A directory with no spec.md is skipped silently in every mode: an
        // evidence-only identity directory is legal (`specs/README.md` §2).
        let opened = match lint::open(directory, &options) {
            Ok(Some(opened)) => opened,
            Ok(None) => continue,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "spec-lint: {error:#}");
                return ExitCode::from(Outcome::Blocking.exit_code());
            }
        };
        match cli.mode() {
            Mode::Check => defects.extend(opened.lint()),
            Mode::Census => {
                let (rows, found) = census::census(
                    &opened.document,
                    &opened.context,
                    opened.artifacts.worklist.as_ref(),
                );
                defects.extend(found);
                rendered.push_str(&census::render(&rows));
            }
            Mode::Coverage => {
                let rows = coverage::coverage(&opened.document, opened.artifacts.trace.as_ref());
                rendered.push_str(&coverage::render(&rows));
            }
        }
    }

    if !rendered.is_empty() && write!(io::stdout().lock(), "{rendered}").is_err() {
        return ExitCode::from(Outcome::Blocking.exit_code());
    }

    let mut stderr = io::stderr().lock();
    for defect in &defects {
        if writeln!(stderr, "{defect}").is_err() {
            return ExitCode::from(Outcome::Blocking.exit_code());
        }
    }

    ExitCode::from(Outcome::of(&defects).exit_code())
}
