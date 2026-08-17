//! What an acceptance criterion requires its lane to write, and whether the
//! task's declared boundary grants it — the reading half of `[L18]`.
//!
//! A task declares exactly one write boundary, its `conflictDomains`, and its
//! acceptance criteria are the oracle it is graded by. A path a criterion
//! requires the tree to carry, to have lost, or to have been rewritten is
//! therefore a path the lane has to write to pass. When that path falls outside
//! the boundary, the task cannot pass its own acceptance without an ownership
//! refusal, and the refusal arrives mid-flight — after the lane has spent its
//! turns deriving what the author already knew.
//!
//! Only write positions count. An argv names far more paths than it writes: a
//! grep pattern, a suite it runs, a build target, a file it merely reads. None
//! of those settles who owns the byte — the file may predate the task entirely
//! — so they stay advisory where admission already reports them. The positions
//! read here are the ones that cannot be true of an unchanged tree:
//!
//! - a redirection target;
//! - an operand of a command whose whole effect is the write: `rm`, `mv`,
//!   `touch`, `mkdir`, `tee`, `cp`, `ln`, `install`, `truncate`, `dd of=`,
//!   `sed -i`;
//! - the path staged, removed, or restored by `git add`, `git rm`, `git mv`,
//!   `git apply`, `git checkout`, `git restore`, `git clean`;
//! - the operand of a `test` file predicate — an acceptance asserting that a
//!   path exists, or that it is gone, is an acceptance asserting the lane put
//!   it there or took it away.

use std::path::{Component, Path};

/// How deep a shell argument carrying another shell argument is followed.
const NESTING: usize = 3;

/// The `test` predicates that ask about a file rather than about a string.
const FILE_PREDICATES: [&str; 19] = [
    "-e", "-f", "-d", "-s", "-x", "-r", "-w", "-L", "-h", "-p", "-S", "-b", "-c", "-g", "-u", "-k",
    "-G", "-O", "-N",
];

/// Words that stand in front of the command without being it.
const WRAPPERS: [&str; 12] = [
    "!", "sudo", "env", "command", "time", "then", "else", "elif", "do", "if", "while", "until",
];

/// Every path an argv requires the lane to have created, rewritten, or deleted,
/// sorted and deduplicated so one path reports once.
pub fn write_targets(argv: &[String]) -> Vec<String> {
    let mut targets = Vec::new();
    for command in commands(argv, 0) {
        targets_of(&command, &mut targets);
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Whether a path falls inside one declared conflict domain. A domain grants
/// itself and everything under it, never the sibling whose name it prefixes:
/// `crates/spec-lint` grants `crates/spec-lint/src/lib.rs` and not
/// `crates/spec-lint-extra/src/lib.rs`.
pub fn inside(path: &str, domain: &str) -> bool {
    let domain = domain.trim().trim_end_matches('/');
    !domain.is_empty()
        && (path == domain
            || path
                .strip_prefix(domain)
                .is_some_and(|rest| rest.starts_with('/')))
}

/// The commands an argv runs. A shell argv carries its real commands inside one
/// script argument, so that argument is split and each command read on its own.
fn commands(argv: &[String], depth: usize) -> Vec<Vec<String>> {
    match script_of(argv) {
        Some(script) if depth < NESTING => split(script)
            .into_iter()
            .flat_map(|command| commands(&command, depth + 1))
            .collect(),
        _ => vec![argv.to_vec()],
    }
}

/// The script argument of a shell argv — the word after the flag carrying `c`.
fn script_of(argv: &[String]) -> Option<&str> {
    let (program, arguments) = argv.split_first()?;
    if !matches!(base_name(program), "bash" | "sh" | "dash" | "zsh") {
        return None;
    }
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if argument.starts_with('-') && argument.contains('c') {
            return arguments.next().map(String::as_str);
        }
    }
    None
}

/// Split a shell script into commands, one word list each. Quoting is honoured,
/// so a quoted pattern stays one word; every control operator ends the command
/// before it, which is all a write position needs to be read in place.
fn split(script: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut characters = script.chars();

    while let Some(character) = characters.next() {
        match quote {
            Some(open) if character == open => quote = None,
            Some(_) => word.push(character),
            None => match character {
                '\'' | '"' => quote = Some(character),
                '\\' => match characters.next() {
                    Some('\n') | None => {}
                    Some(escaped) => word.push(escaped),
                },
                ';' | '&' | '|' | '(' | ')' | '{' | '}' | '\n' => {
                    end(&mut word, &mut words);
                    if !words.is_empty() {
                        commands.push(std::mem::take(&mut words));
                    }
                }
                _ if character.is_whitespace() => end(&mut word, &mut words),
                _ => word.push(character),
            },
        }
    }

    end(&mut word, &mut words);
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

fn end(word: &mut String, words: &mut Vec<String>) {
    if !word.is_empty() {
        words.push(std::mem::take(word));
    }
}

/// The write positions of one command.
fn targets_of(words: &[String], targets: &mut Vec<String>) {
    let mut operands: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < words.len() {
        let word = words[index].as_str();
        if let Some(target) = redirection(word) {
            let target = if target.is_empty() {
                index += 1;
                words.get(index).map_or("", String::as_str)
            } else {
                target
            };
            push(target, targets);
        } else if !word.starts_with('<') {
            // `<file` and `<(command)` read; the command inside a process
            // substitution is read for its own writes by the split above.
            operands.push(word);
        }
        index += 1;
    }

    let operands = unwrapped(&operands);
    let Some((program, arguments)) = operands.split_first() else {
        return;
    };
    match base_name(program) {
        "rm" | "rmdir" | "unlink" | "shred" | "mv" | "touch" | "mkdir" | "truncate" | "tee" => {
            for operand in files(arguments) {
                push(operand, targets);
            }
        }
        // The destination is the written half; the sources are read.
        "cp" | "install" | "ln" => {
            if let Some(destination) = files(arguments).last() {
                push(destination, targets);
            }
        }
        // In place, the first operand is the script and the rest are rewritten.
        "sed" | "perl" | "ruby" if arguments.iter().any(|argument| in_place(argument)) => {
            for operand in files(arguments).into_iter().skip(1) {
                push(operand, targets);
            }
        }
        "dd" => {
            for argument in arguments {
                if let Some(destination) = argument.strip_prefix("of=") {
                    push(destination, targets);
                }
            }
        }
        "git" => git(arguments, targets),
        "test" | "[" | "[[" => {
            let mut arguments = arguments.iter();
            while let Some(argument) = arguments.next() {
                if FILE_PREDICATES.contains(argument) {
                    if let Some(operand) = arguments.next() {
                        push(operand, targets);
                    }
                }
            }
        }
        _ => {}
    }
}

/// The paths a git subcommand writes. Every other subcommand — `diff`, `log`,
/// `show` — names paths it only reads.
fn git(arguments: &[&str], targets: &mut Vec<String>) {
    let words = files(arguments);
    let Some((subcommand, paths)) = words.split_first() else {
        return;
    };
    if matches!(
        *subcommand,
        "add" | "rm" | "mv" | "apply" | "checkout" | "restore" | "clean"
    ) {
        for path in paths {
            push(path, targets);
        }
    }
}

/// The redirection target a word opens, when it opens one. `2>&1` duplicates a
/// descriptor and writes no path.
fn redirection(word: &str) -> Option<&str> {
    let rest = word.trim_start_matches(|character: char| character.is_ascii_digit());
    let rest = rest.strip_prefix('>')?;
    let rest = rest.strip_prefix('>').unwrap_or(rest);
    (!rest.starts_with('&')).then_some(rest)
}

/// The operands with the words that stand in front of the command removed: a
/// negation, a wrapper, an environment assignment, or a runner that hands the
/// rest of the line to another command.
fn unwrapped<'a>(operands: &[&'a str]) -> Vec<&'a str> {
    let mut words = operands.to_vec();
    for _ in 0..NESTING {
        let before = words.len();
        while words
            .first()
            .is_some_and(|word| WRAPPERS.contains(word) || assignment(word))
        {
            words.remove(0);
        }
        // `nix develop --command <argv>` and `nix shell <ref> --command <argv>`
        // run the argv after the flag; the flag itself is nix's.
        if words.first().is_some_and(|word| base_name(word) == "nix") {
            if let Some(at) = words.iter().position(|word| *word == "--command") {
                words = words.split_off(at + 1);
            }
        }
        if words.len() == before {
            break;
        }
    }
    words
}

/// Whether a word is a `NAME=value` environment assignment rather than a path.
fn assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Whether a flag asks `sed` to rewrite its operands in place.
fn in_place(argument: &str) -> bool {
    argument == "--in-place" || (argument.starts_with("-i") && !argument.starts_with("--"))
}

/// The file operands of an argument list: the flags dropped, and everything
/// after `--` taken literally.
fn files<'a>(arguments: &[&'a str]) -> Vec<&'a str> {
    let mut files = Vec::new();
    let mut literal = false;
    for argument in arguments {
        if literal {
            files.push(*argument);
        } else if *argument == "--" {
            literal = true;
        } else if !argument.starts_with('-') {
            files.push(*argument);
        }
    }
    files
}

fn push(token: &str, targets: &mut Vec<String>) {
    if let Some(path) = path_shaped(token) {
        targets.push(path);
    }
}

/// The repo-relative path a token names, when it names one. A token carrying a
/// variable, a glob, a scheme, or a parent-directory hop names no single path
/// this pass can hold against a boundary, and is left to admission.
fn path_shaped(token: &str) -> Option<String> {
    let token = token
        .trim()
        .trim_matches(|character: char| ",;:\"'`".contains(character) || character.is_whitespace());
    let token = token.split('#').next().unwrap_or_default();
    let token = line_reference(token);
    let token = token.trim_start_matches("./").trim_end_matches('/');

    if token.is_empty()
        || token.contains("://")
        || token.contains("//")
        || token.contains(['$', '*', '?', '~'])
        || token.chars().any(char::is_control)
    {
        return None;
    }
    let path = Path::new(token);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return None;
    }
    (token.contains('/') || extended(path)).then(|| token.to_owned())
}

/// A token with a trailing `:<line>` location dropped.
fn line_reference(token: &str) -> &str {
    match token.rsplit_once(':') {
        Some((path, line))
            if !path.is_empty()
                && !line.is_empty()
                && line.chars().all(|character| character.is_ascii_digit()) =>
        {
            path
        }
        _ => token,
    }
}

/// Whether a name carries a file extension: a non-empty stem, then a suffix of
/// alphanumerics and dashes carrying at least one letter.
fn extended(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .rsplit_once('.')
            .is_some_and(|(stem, extension)| {
                !stem.is_empty()
                    && !extension.is_empty()
                    && extension
                        .chars()
                        .all(|character| character.is_ascii_alphanumeric() || character == '-')
                    && extension.chars().any(|character| character.is_alphabetic())
            })
    })
}

fn base_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

#[cfg(test)]
mod tests {
    use super::{inside, write_targets};

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    fn shell(script: &str) -> Vec<String> {
        write_targets(&argv(&["bash", "-lc", script]))
    }

    #[test]
    fn acceptance_domains_read_every_write_position_of_a_shell_line() {
        assert_eq!(
            shell("test -f crates/spec-lint/tests/fixtures/must-fail/expected-defects.json"),
            ["crates/spec-lint/tests/fixtures/must-fail/expected-defects.json"]
        );
        assert_eq!(shell("! test -e drivers/legacy.py"), ["drivers/legacy.py"]);
        assert_eq!(shell("cargo run > doc/report.md"), ["doc/report.md"]);
        assert_eq!(shell("cargo run >>doc/report.md"), ["doc/report.md"]);
        assert_eq!(shell("rm -rf nix/modules/old.nix"), ["nix/modules/old.nix"]);
        assert_eq!(
            shell("mkdir -p crates/spec-lint/tests"),
            ["crates/spec-lint/tests"]
        );
        assert_eq!(shell("cp doc/a.md doc/b.md"), ["doc/b.md"]);
        assert_eq!(shell("sed -i 's/a/b/' flake.nix"), ["flake.nix"]);
        assert_eq!(
            shell("git add crates/spec-lint/src/boundary.rs"),
            ["crates/spec-lint/src/boundary.rs"]
        );
        assert_eq!(
            shell("dd if=/dev/zero of=test/fixtures/blob.bin"),
            ["test/fixtures/blob.bin"]
        );
    }

    #[test]
    fn acceptance_domains_leave_every_read_only_reference_alone() {
        // A pattern, a file grepped, a suite run, a build target, a diff — the
        // path may predate the task, so admission keeps these as advice.
        assert!(shell("grep -n 'trace.json' skills/assign-tally/SKILL.md").is_empty());
        assert!(shell("! grep -rn 'specs/001-' doc/src").is_empty());
        assert!(shell("python3 test/spec_build_driver_test.py").is_empty());
        assert!(shell("git diff --name-only HEAD~1 -- Cargo.toml").is_empty());
        assert!(shell("cat nix/modules/common.nix").is_empty());
        assert!(write_targets(&argv(&[
            "nix",
            "build",
            "--no-link",
            ".#checks.x86_64-linux.module-layer",
        ]))
        .is_empty());
    }

    #[test]
    fn acceptance_domains_read_through_the_wrappers_a_criterion_is_written_with() {
        assert_eq!(
            shell("nix develop --command touch crates/spec-lint/tests/fixtures/new.json"),
            ["crates/spec-lint/tests/fixtures/new.json"]
        );
        assert_eq!(
            shell("TALLY_BIN=target/debug/tally rm drivers/legacy.py"),
            ["drivers/legacy.py"]
        );
        assert_eq!(
            shell("cargo test 2>&1 | tail -20 && test -f doc/out.md"),
            ["doc/out.md"]
        );
        assert_eq!(
            write_targets(&argv(&["touch", "crates/spec-lint/README.md"])),
            ["crates/spec-lint/README.md"]
        );
    }

    #[test]
    fn acceptance_domains_hold_a_path_against_the_domain_that_grants_it() {
        assert!(inside("crates/spec-lint", "crates/spec-lint"));
        assert!(inside("crates/spec-lint/src/lib.rs", "crates/spec-lint"));
        assert!(inside("crates/spec-lint/src/lib.rs", "crates/spec-lint/"));
        assert!(!inside(
            "crates/spec-lint-extra/src/lib.rs",
            "crates/spec-lint"
        ));
        assert!(!inside("flake.nix", "crates/spec-lint"));
        assert!(!inside("crates/spec-lint/src/lib.rs", ""));
    }

    #[test]
    fn acceptance_domains_skip_a_token_that_names_no_single_path() {
        assert!(shell("touch $TMPDIR/scratch.json").is_empty());
        assert!(shell("rm -rf crates/*/target").is_empty());
        assert!(shell("touch /etc/hostname").is_empty());
        assert!(shell("rm ../outside.md").is_empty());
        assert!(shell("touch report").is_empty());
        assert!(shell("tee https://example.invalid/x.json").is_empty());
    }
}
