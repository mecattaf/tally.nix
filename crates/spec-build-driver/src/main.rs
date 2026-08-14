use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use clap::{Parser, ValueEnum};

mod actions;
mod error;
mod git;
mod json;
mod path;
mod sha256;
mod worktrees;

const PY_FALLBACK_ENV: &str = "SPEC_BUILD_PY_FALLBACK";
const DEFAULT_PY_FALLBACK: &str = match option_env!("SPEC_BUILD_PY_FALLBACK") {
    Some(path) => path,
    None => concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../drivers/spec_build_driver.py"
    ),
};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Handler {
    Native,
    PythonFallback,
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

    const fn handler(self) -> Handler {
        match self {
            Self::Worklist
            | Self::Sweep
            | Self::Reconcile
            | Self::Diff
            | Self::SteeringRecheck
            | Self::Steer
            | Self::Retry
            | Self::Escalate
            | Self::Continue
            | Self::Preflight
            | Self::Ownership
            | Self::TreeDelta
            | Self::Constraint
            | Self::Checkpoint
            | Self::Publish
            | Self::Merge => Handler::PythonFallback,
            Self::Prep | Self::Cleanup | Self::Rebase => Handler::Native,
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

fn fallback_path(override_path: Option<OsString>) -> OsString {
    override_path.unwrap_or_else(|| OsString::from(DEFAULT_PY_FALLBACK))
}

fn fallback_command(path: &OsStr, action: Action) -> Command {
    let mut command = Command::new(path);
    command.arg(action.as_str());
    command
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.action.handler() {
        Handler::Native => match actions::load_brief()
            .and_then(|brief| actions::dispatch(cli.action.as_str(), &brief))
        {
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
        },
        Handler::PythonFallback => {
            let fallback = fallback_path(std::env::var_os(PY_FALLBACK_ENV));
            let error = fallback_command(&fallback, cli.action).exec();
            let _write_result = writeln!(
                io::stderr().lock(),
                "spec-build-driver: could not exec Python fallback {} for action {}: {error}",
                Path::new(&fallback).display(),
                cli.action.as_str()
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::{OsStr, OsString};

    use clap::{CommandFactory, Parser, ValueEnum};

    use super::{fallback_command, fallback_path, Action, Cli, Handler, DEFAULT_PY_FALLBACK};

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
            let expected_handler = match expected_name {
                "prep" | "cleanup" | "rebase" => Handler::Native,
                _ => Handler::PythonFallback,
            };
            assert_eq!(parsed.handler(), expected_handler);
        }
    }

    #[test]
    fn only_the_worktree_mechanics_actions_are_native() {
        let native: Vec<_> = Action::value_variants()
            .iter()
            .copied()
            .filter(|action| action.handler() == Handler::Native)
            .map(Action::as_str)
            .collect();
        assert_eq!(native, ["prep", "cleanup", "rebase"]);
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

    #[test]
    fn the_environment_override_wins_over_the_compiled_default() {
        let override_path = OsString::from("/custom/spec-build-driver.py");
        assert_eq!(fallback_path(Some(override_path.clone())), override_path);
        assert_eq!(fallback_path(None), OsString::from(DEFAULT_PY_FALLBACK));
    }

    #[test]
    fn fallback_receives_only_the_selected_action() {
        let command = fallback_command(OsStr::new("/driver.py"), Action::TreeDelta);
        assert_eq!(command.get_program(), OsStr::new("/driver.py"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [OsStr::new("treeDelta")]
        );
    }
}
