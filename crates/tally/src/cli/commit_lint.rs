use super::*;

use std::collections::BTreeSet;
use std::process::Command as ProcessCommand;

use serde::Serialize;

const COMMIT_TYPES: [&str; 8] = [
    "feat", "fix", "refactor", "docs", "build", "chore", "test", "gate",
];
const MAX_HEADER_CHARS: usize = 72;
const MAX_BODY_LINE_CHARS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConventionalHeader {
    pub(super) kind: String,
    pub(super) scope: Option<String>,
    pub(super) breaking: bool,
    pub(super) subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct CommitViolation {
    rule: &'static str,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CommitValidation {
    pub(super) header: Option<ConventionalHeader>,
    pub(super) violations: Vec<CommitViolation>,
}

impl CommitValidation {
    pub(super) fn is_valid(&self) -> bool {
        self.violations.is_empty()
    }
}

#[derive(Debug)]
struct HistoryCommit {
    object_id: String,
    message: String,
}

#[derive(Serialize)]
struct HistoryVerdict<'a> {
    commit: &'a str,
    header: &'a str,
    verdict: &'static str,
    violations: &'a [CommitViolation],
}

#[derive(Default)]
struct TrailerParagraph<'a> {
    valid: bool,
    has_entry: bool,
    has_policy_entry: bool,
    has_known_malformed: bool,
    first_trailer: Option<usize>,
    keys: Vec<&'a str>,
}

pub(super) fn run_lint_history(args: LintHistoryArgs) -> Result<()> {
    let scopes = scope_vocabulary(args.scopes)?;
    let commits = git_history(&args.range)?;
    let mut failed = 0usize;

    for commit in &commits {
        let validation = validate_commit_message(&commit.message, &scopes);
        if !validation.is_valid() {
            failed += 1;
        }
        let verdict = HistoryVerdict {
            commit: &commit.object_id,
            header: commit_header(&commit.message),
            verdict: if validation.is_valid() {
                "pass"
            } else {
                "fail"
            },
            violations: &validation.violations,
        };
        outln!("{}", serde_json::to_string(&verdict)?);
    }

    if failed == 0 {
        Ok(())
    } else {
        Err(exit_failure(
            1,
            format!("{failed} of {} commit(s) failed validation", commits.len()),
        ))
    }
}

fn scope_vocabulary(scopes: Vec<String>) -> Result<BTreeSet<String>> {
    let mut vocabulary = BTreeSet::new();
    for scope in scopes {
        if !valid_scope(&scope) {
            return Err(invalid(format!(
                "scope {scope:?} must be a non-empty conventional-commit scope"
            )));
        }
        vocabulary.insert(scope);
    }
    Ok(vocabulary)
}

fn git_history(range: &str) -> Result<Vec<HistoryCommit>> {
    if range.is_empty() || range.contains(['\0', '\n', '\r']) {
        return Err(invalid(
            "lint-history RANGE must be one non-empty Git revision expression",
        ));
    }
    let output = ProcessCommand::new("git")
        .args(["log", "--reverse", "-z", "--format=%H%x00%B", range, "--"])
        .output()
        .context("running git log for lint-history")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git log rejected range {range:?}: {}", stderr.trim_end());
    }
    let output = String::from_utf8(output.stdout).context("git log output was not UTF-8")?;
    let mut fields = output.split('\0').collect::<Vec<_>>();
    if fields.last() == Some(&"") {
        fields.pop();
    }
    if fields.is_empty() {
        return Err(invalid(format!(
            "lint-history range {range:?} selected no commits"
        )));
    }
    if fields.len() % 2 != 0 {
        bail!("git log returned malformed lint-history output");
    }
    fields
        .chunks_exact(2)
        .map(|fields| {
            let object_id = fields[0];
            if object_id.len() < 7 || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("git log returned a malformed commit object id");
            }
            Ok(HistoryCommit {
                object_id: object_id.to_owned(),
                message: fields[1].to_owned(),
            })
        })
        .collect()
}

pub(super) fn validate_commit_message(
    message: &str,
    scopes: &BTreeSet<String>,
) -> CommitValidation {
    let normalized = message.replace("\r\n", "\n");
    let mut lines = normalized.split('\n').collect::<Vec<_>>();
    while lines.last() == Some(&"") {
        lines.pop();
    }
    let header = lines.first().copied().unwrap_or_default();
    let mut violations = Vec::new();
    let parsed = parse_header(header, scopes, &mut violations);

    let paragraphs = message_paragraphs(&lines);
    let analyses = paragraphs
        .iter()
        .map(|&(start, end)| analyze_trailer_paragraph(&lines, start, end))
        .collect::<Vec<_>>();
    let footer_start = analyses
        .last()
        .filter(|analysis| analysis.valid)
        .and_then(|_| paragraphs.last().map(|paragraph| paragraph.0));

    for (index, analysis) in analyses.iter().enumerate() {
        let is_final = index + 1 == analyses.len();
        if (!is_final && analysis.valid)
            || analysis.has_known_malformed
            || (analysis.has_policy_entry && !analysis.valid)
        {
            add_violation(
                &mut violations,
                "trailer-block-wellformed",
                "trailers must form one contiguous block at the end of the message",
            );
        }
    }

    let final_analysis = analyses.last();
    let first_footer_like = final_analysis.and_then(|analysis| analysis.first_trailer);
    if first_footer_like.is_some_and(|index| index == 1 || !lines[index - 1].is_empty()) {
        add_violation(
            &mut violations,
            "footer-leading-blank",
            "the trailer block must be preceded by a blank line",
        );
    }

    let body_end = footer_start.unwrap_or(lines.len());
    let body_exists = lines
        .get(1..body_end)
        .is_some_and(|body| body.iter().any(|line| !line.is_empty()));
    if body_exists && lines.get(1).is_some_and(|line| !line.is_empty()) {
        add_violation(
            &mut violations,
            "body-leading-blank",
            "the body must be preceded by a blank line",
        );
    }
    for line in lines.get(1..body_end).unwrap_or_default() {
        if line.chars().count() > MAX_BODY_LINE_CHARS {
            add_violation(
                &mut violations,
                "body-max-line-length",
                "body lines must be at most 100 characters",
            );
            break;
        }
    }

    for line in lines.iter().skip(1) {
        let Some((key, _)) = line.split_once(':') else {
            continue;
        };
        if key.eq_ignore_ascii_case("Co-authored-by") {
            add_violation(
                &mut violations,
                "trailer-block-wellformed",
                "use Assisted-by instead of Co-authored-by",
            );
        }
        if key.eq_ignore_ascii_case("BREAKING CHANGE") {
            add_violation(
                &mut violations,
                "trailer-block-wellformed",
                "use the hyphenated BREAKING-CHANGE trailer token",
            );
        }
        if key.eq_ignore_ascii_case("BREAKING-CHANGE") && key != "BREAKING-CHANGE" {
            add_violation(
                &mut violations,
                "trailer-block-wellformed",
                "BREAKING-CHANGE must use its canonical uppercase spelling",
            );
        }
    }
    if final_analysis.is_some_and(|analysis| {
        analysis.keys.contains(&"BREAKING-CHANGE")
            && !parsed.as_ref().is_some_and(|header| header.breaking)
    }) {
        add_violation(
            &mut violations,
            "trailer-block-wellformed",
            "BREAKING-CHANGE is only valid on a header containing !",
        );
    }

    CommitValidation {
        header: parsed,
        violations,
    }
}

pub(super) fn commit_header(message: &str) -> &str {
    let header = message.split('\n').next().unwrap_or_default();
    header.strip_suffix('\r').unwrap_or(header)
}

fn parse_header(
    header: &str,
    scopes: &BTreeSet<String>,
    violations: &mut Vec<CommitViolation>,
) -> Option<ConventionalHeader> {
    if header.chars().count() > MAX_HEADER_CHARS {
        add_violation(
            violations,
            "header-max-length",
            "the header must be at most 72 characters",
        );
    }
    let Some((prefix, subject)) = header.split_once(": ") else {
        add_violation(
            violations,
            "type-enum",
            "the header must have the form type(scope)!: subject",
        );
        return None;
    };
    let breaking = prefix.ends_with('!');
    let prefix = prefix.strip_suffix('!').unwrap_or(prefix);
    let (kind, scope) = match prefix.find('(') {
        Some(open)
            if open > 0
                && prefix.ends_with(')')
                && !prefix[open + 1..prefix.len() - 1].contains(['(', ')']) =>
        {
            let scope = &prefix[open + 1..prefix.len() - 1];
            (&prefix[..open], Some(scope))
        }
        Some(_) => {
            add_violation(
                violations,
                "scope-enum",
                "the scope must be one non-empty value in parentheses",
            );
            return None;
        }
        None if prefix.contains(')') => {
            add_violation(
                violations,
                "scope-enum",
                "the scope must be one non-empty value in parentheses",
            );
            return None;
        }
        None => (prefix, None),
    };
    if !COMMIT_TYPES.contains(&kind) {
        add_violation(
            violations,
            "type-enum",
            "type must be feat, fix, refactor, docs, build, chore, test, or gate",
        );
    }
    if let Some(scope) = scope {
        if !valid_scope(scope) || !scopes.contains(scope) {
            add_violation(
                violations,
                "scope-enum",
                "scope is not in the vocabulary supplied for this invocation",
            );
        }
    }
    if subject.is_empty()
        || subject.trim() != subject
        || subject
            .chars()
            .any(|character| character.is_control() || character.is_uppercase())
    {
        add_violation(
            violations,
            "subject-case",
            "the subject must be non-empty and lowercase",
        );
    }
    if subject.trim_end().ends_with('.') {
        add_violation(
            violations,
            "subject-full-stop",
            "the subject must not end with a period",
        );
    }
    Some(ConventionalHeader {
        kind: kind.to_owned(),
        scope: scope.map(str::to_owned),
        breaking,
        subject: subject.to_owned(),
    })
}

fn valid_scope(scope: &str) -> bool {
    !scope.is_empty()
        && scope.chars().all(|character| {
            !character.is_control()
                && !character.is_whitespace()
                && !matches!(character, '(' | ')' | ':' | '!')
        })
}

fn message_paragraphs(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut paragraphs = Vec::new();
    let mut index = 1usize;
    while index < lines.len() {
        while index < lines.len() && lines[index].is_empty() {
            index += 1;
        }
        let start = index;
        while index < lines.len() && !lines[index].is_empty() {
            index += 1;
        }
        if start < index {
            paragraphs.push((start, index));
        }
    }
    paragraphs
}

fn analyze_trailer_paragraph<'a>(
    lines: &[&'a str],
    start: usize,
    end: usize,
) -> TrailerParagraph<'a> {
    let mut analysis = TrailerParagraph {
        valid: true,
        ..TrailerParagraph::default()
    };
    for (index, line) in lines.iter().enumerate().take(end).skip(start) {
        if line.starts_with([' ', '\t']) {
            if !analysis.has_entry || line.trim().is_empty() {
                analysis.valid = false;
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            analysis.valid = false;
            continue;
        };
        let token = !key.is_empty()
            && key
                .bytes()
                .enumerate()
                .all(|(index, byte)| byte.is_ascii_alphanumeric() || (index > 0 && byte == b'-'));
        let policy_key = is_policy_trailer_key(key) || key.eq_ignore_ascii_case("BREAKING CHANGE");
        let trailer_value = value.trim_start_matches([' ', '\t']);
        if !token || trailer_value.len() == value.len() || trailer_value.trim().is_empty() {
            analysis.valid = false;
            analysis.has_known_malformed |= policy_key;
            if policy_key && analysis.first_trailer.is_none() {
                analysis.first_trailer = Some(index);
            }
            continue;
        }
        analysis.has_entry = true;
        analysis.has_policy_entry |= policy_key;
        analysis.first_trailer.get_or_insert(index);
        analysis.keys.push(key);
    }
    analysis.valid &= analysis.has_entry;
    analysis
}

fn is_policy_trailer_key(key: &str) -> bool {
    [
        "Tally-Task",
        "Tally-Revision",
        "Tally-Receipt",
        "Assisted-by",
        "Fixes",
        "Closes",
        "BREAKING-CHANGE",
        "Co-authored-by",
    ]
    .iter()
    .any(|known| key.eq_ignore_ascii_case(known))
}

fn add_violation(violations: &mut Vec<CommitViolation>, rule: &'static str, message: &'static str) {
    if !violations.iter().any(|violation| violation.rule == rule) {
        violations.push(CommitViolation {
            rule,
            message: message.to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scopes() -> BTreeSet<String> {
        BTreeSet::from(["crates/tally".to_owned(), "drivers".to_owned()])
    }

    fn rules(validation: &CommitValidation) -> BTreeSet<&'static str> {
        validation
            .violations
            .iter()
            .map(|violation| violation.rule)
            .collect()
    }

    #[test]
    fn complete_commit_message_is_valid() {
        let message = "feat(crates/tally)!: validate commit history\n\nExplain why the history audit is needed.\n\nTally-Task: commit-validator\nTally-Revision: sha256:abc\nBREAKING-CHANGE: rerun the release plan";
        let validation = validate_commit_message(message, &scopes());
        assert_eq!(validation.violations, []);
        assert_eq!(validation.header.unwrap().kind, "feat");
    }

    #[test]
    fn all_closed_commit_types_are_accepted() {
        for kind in COMMIT_TYPES {
            let validation = validate_commit_message(&format!("{kind}: audit history"), &scopes());
            assert!(validation.is_valid(), "{kind}: {:?}", validation.violations);
        }
    }

    #[test]
    fn header_and_body_rules_are_reported_together() {
        let long_subject = format!("Fix history. {}", "x".repeat(80));
        let long_body = "b".repeat(101);
        let message = format!("unknown(drivers): {long_subject}\n{long_body}");
        let found = rules(&validate_commit_message(&message, &scopes()));
        assert!(found.contains("type-enum"));
        assert!(found.contains("subject-case"));
        assert!(!found.contains("subject-full-stop"));
        assert!(found.contains("header-max-length"));
        assert!(found.contains("body-leading-blank"));
        assert!(found.contains("body-max-line-length"));
    }

    #[test]
    fn unknown_scope_and_final_period_are_rejected() {
        let validation = validate_commit_message("fix(other): repair history.", &scopes());
        let found = rules(&validation);
        assert!(found.contains("scope-enum"));
        assert!(found.contains("subject-full-stop"));
    }

    #[test]
    fn trailer_block_poisoning_is_rejected() {
        let message = "fix(crates/tally): reject poisoned trailers\n\nTally-Task: validator\nthis line poisons the trailer paragraph\nTally-Revision: sha256:abc";
        let found = rules(&validate_commit_message(message, &scopes()));
        assert!(found.contains("trailer-block-wellformed"));
    }

    #[test]
    fn trailers_must_be_one_final_block_with_a_leading_blank() {
        let split = "fix(crates/tally): audit trailers\n\nTally-Task: validator\n\nTally-Revision: sha256:abc";
        assert!(
            rules(&validate_commit_message(split, &scopes())).contains("trailer-block-wellformed")
        );

        let attached = "fix(crates/tally): audit trailers\nTally-Task: validator";
        assert!(
            rules(&validate_commit_message(attached, &scopes())).contains("footer-leading-blank")
        );
    }

    #[test]
    fn breaking_change_is_hyphenated_and_requires_bang() {
        let spaced = "feat(crates/tally)!: change grammar\n\nBREAKING CHANGE: migrate now";
        assert!(
            rules(&validate_commit_message(spaced, &scopes())).contains("trailer-block-wellformed")
        );

        let missing_bang = "feat(crates/tally): change grammar\n\nBREAKING-CHANGE: migrate now";
        assert!(rules(&validate_commit_message(missing_bang, &scopes()))
            .contains("trailer-block-wellformed"));
    }

    #[test]
    fn co_author_trailers_are_not_part_of_the_policy_block() {
        let message = "chore(crates/tally): record assistance\n\nCo-authored-by: Model <model@example.invalid>";
        assert!(rules(&validate_commit_message(message, &scopes()))
            .contains("trailer-block-wellformed"));
    }
}
