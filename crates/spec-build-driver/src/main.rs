use std::io::{self, Write};
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

mod actions;
mod error;
mod git;
mod json;
mod path;
mod sha256;
mod worktrees;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
enum Action {
    #[value(name = "worklist")]
    Worklist,
    #[value(name = "sweep")]
    Sweep,
    #[value(name = "reconcile")]
    Reconcile,
    #[value(name = "diff")]
    Diff,
    #[value(name = "steeringRecheck")]
    SteeringRecheck,
    #[value(name = "steer")]
    Steer,
    #[value(name = "retry")]
    Retry,
    #[value(name = "escalate")]
    Escalate,
    #[value(name = "continue")]
    Continue,
    #[value(name = "preflight")]
    Preflight,
    #[value(name = "prep")]
    Prep,
    #[value(name = "cleanup")]
    Cleanup,
    #[value(name = "ownership")]
    Ownership,
    #[value(name = "treeDelta")]
    TreeDelta,
    #[value(name = "constraint")]
    Constraint,
    #[value(name = "checkpoint")]
    Checkpoint,
    #[value(name = "publish")]
    Publish,
    #[value(name = "rebase")]
    Rebase,
    #[value(name = "merge")]
    Merge,
}

impl Action {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Worklist => "worklist",
            Self::Sweep => "sweep",
            Self::Reconcile => "reconcile",
            Self::Diff => "diff",
            Self::SteeringRecheck => "steeringRecheck",
            Self::Steer => "steer",
            Self::Retry => "retry",
            Self::Escalate => "escalate",
            Self::Continue => "continue",
            Self::Preflight => "preflight",
            Self::Prep => "prep",
            Self::Cleanup => "cleanup",
            Self::Ownership => "ownership",
            Self::TreeDelta => "treeDelta",
            Self::Constraint => "constraint",
            Self::Checkpoint => "checkpoint",
            Self::Publish => "publish",
            Self::Rebase => "rebase",
            Self::Merge => "merge",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "spec-build-driver",
    about = "Run one spec-build campaign action"
)]
struct Cli {
    #[arg(value_enum)]
    action: Action,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match actions::load_brief().and_then(|brief| actions::dispatch(cli.action.as_str(), &brief)) {
        Ok(result) => {
            let written = writeln!(
                io::stdout().lock(),
                "TALLY_FINAL_MESSAGE={}",
                result.stringify()
            );
            if let Err(error) = written {
                let _ = writeln!(
                    io::stderr().lock(),
                    "spec-build-driver: cannot emit final message: {error}"
                );
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            let _ = writeln!(io::stderr().lock(), "spec-build-driver: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser, ValueEnum};

    use super::{Action, Cli};

    const ACTION_NAMES: [&str; 19] = [
        "worklist",
        "sweep",
        "reconcile",
        "diff",
        "steeringRecheck",
        "steer",
        "retry",
        "escalate",
        "continue",
        "preflight",
        "prep",
        "cleanup",
        "ownership",
        "treeDelta",
        "constraint",
        "checkpoint",
        "publish",
        "rebase",
        "merge",
    ];

    #[test]
    fn every_action_is_accepted_and_dispatched() {
        let variants = Action::value_variants();
        assert_eq!(variants.len(), ACTION_NAMES.len());

        for (variant, expected_name) in variants.iter().zip(ACTION_NAMES) {
            let parsed = Cli::try_parse_from(["spec-build-driver", expected_name])
                .expect("the Python driver's action must remain accepted")
                .action;
            assert_eq!(parsed, *variant);
            assert_eq!(parsed.as_str(), expected_name);
        }
    }

    #[test]
    fn help_lists_every_action() {
        let mut help = Vec::new();
        Cli::command()
            .write_long_help(&mut help)
            .expect("help should render");
        let help = String::from_utf8(help).expect("clap help is UTF-8");

        for action in ACTION_NAMES {
            assert!(help.contains(action), "help omitted action {action}");
        }
    }
}
