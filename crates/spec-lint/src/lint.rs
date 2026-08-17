//! The check pass: every rule `specs/README.md` §7 marks as a single-spec rule,
//! run over one `specs/<identity>/spec.md`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context as _;
use regex::Regex;

use crate::claim::{self, Believe};
use crate::defect::Defect;
use crate::document::{Document, Line, Section, NEVER_OMITTABLE, SECTION_ORDER};
use crate::lexicon;
use crate::rules::RuleId;
use crate::tree::{self, Tree};

/// What one `spec.md` is read against.
#[derive(Clone, Debug)]
pub struct Context {
    /// The path printed on every defect line.
    pub file: String,
    /// The directory name — the join key `specs/README.md` §2 fixes.
    pub identity: String,
    /// The identity directory itself, for the files a spec cites beside it.
    pub directory: PathBuf,
    /// The working-tree root paths resolve against.
    pub root: PathBuf,
}

/// Lint one identity directory. `Ok(None)` means the directory carries no
/// `spec.md` and is skipped silently, as §2 requires of evidence-only dirs.
pub fn lint_directory(
    directory: &Path,
    root: Option<&Path>,
) -> anyhow::Result<Option<Vec<Defect>>> {
    let spec = directory.join("spec.md");
    if !spec.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&spec)
        .with_context(|| format!("cannot read {}", spec.display()))?;
    let canonical = directory
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", directory.display()))?;
    let identity = canonical
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let context = Context {
        file: spec.display().to_string(),
        identity,
        directory: directory.to_path_buf(),
        root: root.map_or_else(|| tree::infer_root(directory), Path::to_path_buf),
    };
    Ok(Some(lint_text(&text, &context)))
}

/// Lint the bytes of one `spec.md`.
pub fn lint_text(text: &str, context: &Context) -> Vec<Defect> {
    let document = Document::parse(text);
    let mut tree = Tree::new(context.root.clone()).with_local(context.directory.clone());
    let mut defects = Vec::new();

    let status = status_block(&document, context, &mut defects);
    let bodies = section_set(&document, context, &mut defects);
    let vocabulary = vocabulary(&document, context, &bodies, &mut defects);
    rulings(&document, context, &bodies, &mut defects);
    prime_believed_files(&document, &mut tree);
    claims(
        &document,
        context,
        &bodies,
        &vocabulary,
        &tree,
        &mut defects,
    );
    unchanged(
        &document,
        context,
        &bodies,
        &vocabulary,
        &tree,
        &mut defects,
    );
    unknowns(&document, context, &bodies, &mut defects);
    stages(&document, context, &bodies, &mut defects);
    forbidden(
        &document,
        context,
        &bodies,
        &vocabulary,
        &tree,
        &mut defects,
    );
    doubt(&document, context, status.ratified, &mut defects);
    lexical(&document, context, &mut defects);

    defects.sort();
    defects.dedup();
    defects
}

/// What the status block says about the spec's lifecycle.
#[derive(Clone, Debug, Default)]
struct Status {
    ratified: bool,
}

/// The state of one section's body.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Body {
    Missing,
    Empty,
    Omitted,
    Content,
}

type Bodies = BTreeMap<String, Body>;

fn status_block(document: &Document, context: &Context, defects: &mut Vec<Defect>) -> Status {
    const KEYS: [&str; 4] = ["Status", "Governs", "Consumers", "Supersedes"];
    static TITLE: OnceLock<Regex> = OnceLock::new();
    static KEY: OnceLock<Regex> = OnceLock::new();
    static STATUS: OnceLock<Regex> = OnceLock::new();
    let title = TITLE.get_or_init(|| Regex::new(r"^# (\S+) — (.+)$").expect("compiles"));
    let key = KEY.get_or_init(|| Regex::new(r"^([A-Za-z]+):(.*)$").expect("compiles"));
    let status_value = STATUS.get_or_init(|| {
        Regex::new(r"^(proposed|ratified [0-9]{4}-[0-9]{2}-[0-9]{2}|closed \S.*)$")
            .expect("compiles")
    });

    let mut status = Status::default();
    let lines: Vec<&Line> = document
        .preamble()
        .iter()
        .filter(|line| !line.is_blank() && !line.fenced)
        .collect();

    let Some((first, rest)) = lines.split_first() else {
        defects.push(Defect::blocking(
            &context.file,
            1,
            RuleId::L2,
            "the status block is missing; a spec opens with `# <identity> — <title>`",
        ));
        return status;
    };

    match title.captures(first.trimmed()) {
        None => defects.push(Defect::blocking(
            &context.file,
            first.number,
            RuleId::L2,
            "the title line must read `# <identity> — <title>`",
        )),
        Some(captured) if captured[1] != context.identity => defects.push(Defect::blocking(
            &context.file,
            first.number,
            RuleId::L2,
            format!(
                "the title names identity `{}`; the directory is `{}`",
                &captured[1], context.identity
            ),
        )),
        Some(_) => {}
    }

    let mut seen: Vec<(&str, &Line, String)> = Vec::new();
    for line in rest {
        match key.captures(line.trimmed()) {
            Some(captured) if KEYS.contains(&&captured[1]) => {
                let name = KEYS
                    .into_iter()
                    .find(|candidate| *candidate == &captured[1])
                    .expect("the key was just matched");
                if seen.iter().any(|(other, _, _)| *other == name) {
                    defects.push(Defect::blocking(
                        &context.file,
                        line.number,
                        RuleId::L2,
                        format!("the status block repeats `{name}:`"),
                    ));
                }
                seen.push((name, line, captured[2].trim().to_owned()));
            }
            _ => defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L2,
                "the status block admits `Status:`, `Governs:`, `Consumers:`, and `Supersedes:` only",
            )),
        }
    }

    for (position, name) in KEYS.into_iter().enumerate() {
        let Some((_, line, value)) = seen.iter().find(|(other, _, _)| *other == name) else {
            defects.push(Defect::blocking(
                &context.file,
                first.number,
                RuleId::L2,
                format!("the status block has no `{name}:` line"),
            ));
            continue;
        };
        let observed = seen
            .iter()
            .position(|(other, _, _)| *other == name)
            .expect("the key was just found");
        let out_of_order = seen
            .iter()
            .take(observed)
            .any(|(other, _, _)| KEYS.iter().position(|k| k == other) > Some(position));
        if out_of_order {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L2,
                format!(
                    "the status block lines read `Status:`, `Governs:`, `Consumers:`, `Supersedes:`; `{name}:` is out of order"
                ),
            ));
        }

        match name {
            "Status" => {
                if status_value.is_match(value) {
                    status.ratified = value.starts_with("ratified");
                } else {
                    defects.push(Defect::blocking(
                        &context.file,
                        line.number,
                        RuleId::L2,
                        "Status reads `proposed`, `ratified YYYY-MM-DD`, or `closed <release-ref>`",
                    ));
                }
            }
            "Governs" => {
                let expected = format!("silent-factory-worklists/{}.json", context.identity);
                if *value != expected {
                    defects.push(Defect::blocking(
                        &context.file,
                        line.number,
                        RuleId::L2,
                        format!("Governs must name `{expected}`"),
                    ));
                }
            }
            "Consumers" => {
                if value.is_empty() {
                    defects.push(Defect::blocking(
                        &context.file,
                        line.number,
                        RuleId::L2,
                        "Consumers names at least one gate, check attribute, skill, or sitting",
                    ));
                }
            }
            _ => {
                if value.is_empty() {
                    defects.push(Defect::blocking(
                        &context.file,
                        line.number,
                        RuleId::L2,
                        "Supersedes reads `<path>` or `none`",
                    ));
                }
            }
        }
    }

    if status.ratified {
        if let Some((_, line, value)) = seen.iter().find(|(name, _, _)| *name == "Governs") {
            if !Tree::new(context.root.clone()).exists(value) {
                defects.push(Defect::blocking(
                    &context.file,
                    line.number,
                    RuleId::L2,
                    format!("Governs names `{value}`, which does not exist at this revision"),
                ));
            }
        }
    }

    status
}

fn section_set(document: &Document, context: &Context, defects: &mut Vec<Defect>) -> Bodies {
    let mut bodies = Bodies::new();
    let mut highest: Option<usize> = None;

    for section in &document.sections {
        let Some(position) = SECTION_ORDER.iter().position(|name| *name == section.name) else {
            defects.push(Defect::blocking(
                &context.file,
                section.heading,
                RuleId::L1,
                format!(
                    "`## {}` is not one of the eight sections `specs/README.md` §3 fixes",
                    section.name
                ),
            ));
            continue;
        };
        if highest.is_some_and(|previous| position <= previous) {
            defects.push(Defect::blocking(
                &context.file,
                section.heading,
                RuleId::L1,
                format!(
                    "`## {}` is out of order; the section set runs {}",
                    section.name,
                    SECTION_ORDER.join(", ")
                ),
            ));
        }
        highest = Some(highest.map_or(position, |previous| previous.max(position)));
        bodies.insert(
            section.name.clone(),
            body_state(document, section, context, defects),
        );
    }

    for name in SECTION_ORDER {
        if bodies.contains_key(name) {
            continue;
        }
        bodies.insert(name.to_owned(), Body::Missing);
        if NEVER_OMITTABLE.contains(&name) {
            defects.push(Defect::blocking(
                &context.file,
                1,
                RuleId::L1,
                format!("the `## {name}` section is never omittable"),
            ));
        } else {
            defects.push(Defect::blocking(
                &context.file,
                1,
                RuleId::L15,
                format!(
                    "the `## {name}` section is missing; keep the heading with the single body line `Omitted: <one-line reason>.`"
                ),
            ));
        }
    }

    bodies
}

fn body_state(
    document: &Document,
    section: &Section,
    context: &Context,
    defects: &mut Vec<Defect>,
) -> Body {
    let content = content_lines(document, section);
    let Some(first) = content.first() else {
        if NEVER_OMITTABLE.contains(&section.name.as_str()) {
            defects.push(Defect::blocking(
                &context.file,
                section.heading,
                RuleId::L1,
                format!("the `## {}` section is never omittable", section.name),
            ));
        } else if section.name == "Rulings" {
            defects.push(Defect::warning(
                &context.file,
                section.heading,
                RuleId::L15,
                "the Rulings section is empty; every ambiguity resolved while authoring gets a row",
            ));
        } else {
            defects.push(Defect::blocking(
                &context.file,
                section.heading,
                RuleId::L15,
                format!(
                    "the `## {}` section is empty; omit it with the single body line `Omitted: <one-line reason>.`",
                    section.name
                ),
            ));
        }
        return Body::Empty;
    };

    if !first.trimmed().trim_start().starts_with("Omitted:") {
        return Body::Content;
    }

    if content.len() > 1 || !first.trimmed().trim_end().ends_with('.') {
        defects.push(Defect::blocking(
            &context.file,
            first.number,
            RuleId::L1,
            "an omitted section keeps its heading and one body line `Omitted: <one-line reason>.`",
        ));
    }
    if NEVER_OMITTABLE.contains(&section.name.as_str()) {
        defects.push(Defect::blocking(
            &context.file,
            first.number,
            RuleId::L1,
            format!("the `## {}` section is never omittable", section.name),
        ));
    } else if section.name == "Rulings" {
        defects.push(Defect::warning(
            &context.file,
            first.number,
            RuleId::L15,
            "the Rulings section is omitted; every ambiguity resolved while authoring gets a row",
        ));
    }
    Body::Omitted
}

/// One logical line of a section body: a line plus the indented continuation
/// lines that wrap it. The grammar is line-oriented; wrapping a long line at
/// the margin is typography, not a second line.
#[derive(Clone, Debug)]
struct Logical {
    number: usize,
    text: String,
}

impl Logical {
    fn trimmed(&self) -> &str {
        &self.text
    }
}

fn content_lines(document: &Document, section: &Section) -> Vec<Logical> {
    let mut logical: Vec<Logical> = Vec::new();
    for line in document.body(section) {
        if line.is_blank() || line.fenced {
            continue;
        }
        let continuation = line.text.starts_with([' ', '\t']);
        match (continuation, logical.last_mut()) {
            (true, Some(previous)) => {
                previous.text.push(' ');
                previous.text.push_str(line.text.trim());
            }
            _ => logical.push(Logical {
                number: line.number,
                text: line.text.trim().to_owned(),
            }),
        }
    }
    logical
}

/// The Vocabulary section, indexed for the identifier and drift rules.
#[derive(Clone, Debug, Default)]
struct Vocabulary {
    terms: Vec<String>,
    text: String,
}

impl Vocabulary {
    fn carries(&self, token: &str) -> bool {
        let lowered = token.to_lowercase();
        self.text.to_lowercase().contains(&lowered)
            || self.terms.iter().any(|term| {
                term.to_lowercase()
                    .split_whitespace()
                    .any(|word| word == lowered)
            })
    }
}

fn vocabulary(
    document: &Document,
    context: &Context,
    bodies: &Bodies,
    defects: &mut Vec<Defect>,
) -> Vocabulary {
    static LINE: OnceLock<Regex> = OnceLock::new();
    let pattern =
        LINE.get_or_init(|| Regex::new(r"^- (.+?)( \(NEW\))? — (.+)$").expect("compiles"));

    let mut vocabulary = Vocabulary::default();
    let Some(section) = document.section("Vocabulary") else {
        return vocabulary;
    };
    if bodies.get("Vocabulary") != Some(&Body::Content) {
        return vocabulary;
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut declared: Vec<(String, usize)> = Vec::new();
    for line in content_lines(document, section) {
        vocabulary.text.push_str(line.trimmed());
        vocabulary.text.push('\n');
        let Some(captured) = pattern.captures(line.trimmed()) else {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L11,
                "a vocabulary line reads `- <term> — <definition>`, with an optional `(NEW)` flag",
            ));
            continue;
        };
        let term = captured[1].trim().to_owned();
        if !seen.insert(term.to_lowercase()) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L11,
                format!("`{term}` is defined twice; one noun per concept, declared once"),
            ));
        }
        declared.push((term.clone(), line.number));
        vocabulary.terms.push(term);
    }

    let elsewhere: String = document
        .lines
        .iter()
        .filter(|line| !(section.body.contains(&(line.number - 1))))
        .map(|line| line.text.to_lowercase())
        .collect::<Vec<String>>()
        .join("\n");
    for (term, line) in declared {
        if !elsewhere.contains(&term.to_lowercase()) {
            defects.push(Defect::warning(
                &context.file,
                line,
                RuleId::L11,
                format!("`{term}` is defined and never used"),
            ));
        }
    }

    vocabulary
}

fn rulings(document: &Document, context: &Context, bodies: &Bodies, defects: &mut Vec<Defect>) {
    static ID: OnceLock<Regex> = OnceLock::new();
    let id = ID.get_or_init(|| Regex::new(r"^[A-Z][0-9]+$").expect("compiles"));
    static GROUP: OnceLock<Regex> = OnceLock::new();
    let group = GROUP.get_or_init(|| Regex::new(r"^R[0-9]+$").expect("compiles"));

    let Some(section) = document.section("Rulings") else {
        return;
    };
    if bodies.get("Rulings") != Some(&Body::Content) {
        return;
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut rows = 0;
    for (position, line) in content_lines(document, section).into_iter().enumerate() {
        let text = line.trimmed().trim();
        if !text.starts_with('|') {
            continue;
        }
        if position < 2 {
            continue;
        }
        let cell = text
            .trim_matches('|')
            .split('|')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned();
        rows += 1;
        if !id.is_match(&cell) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("ruling id `{cell}` must match `[A-Z][0-9]+`"),
            ));
        } else if group.is_match(&cell) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("ruling id `{cell}` collides with the `R[0-9]+` claim-group namespace"),
            ));
        }
        if !seen.insert(cell.clone()) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("ruling id `{cell}` is used twice"),
            ));
        }
    }

    if rows == 0 {
        defects.push(Defect::warning(
            &context.file,
            section.heading,
            RuleId::L15,
            "the Rulings section has no rows; every ambiguity resolved while authoring gets a row",
        ));
    }
}

fn prime_believed_files(document: &Document, tree: &mut Tree) {
    for line in &document.lines {
        if line.fenced {
            continue;
        }
        let Some(offset) = line.text.find("BELIEVE:") else {
            continue;
        };
        if let Believe::Mark(path) = claim::parse(&line.text[offset..]).believe {
            tree.believe(&path);
        }
    }
}

fn arrow_line(
    context: &Context,
    line: &Logical,
    body: &str,
    vocabulary: &Vocabulary,
    tree: &Tree,
    defects: &mut Vec<Defect>,
) {
    let parsed = claim::parse(body);

    if parsed.arrows == 0 {
        defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L4,
            "the arrow `→` is mandatory: `<condition> → <observable>`",
        ));
    } else if parsed.arrows > 1 {
        defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L4,
            "exactly one claim per line; this line carries more than one `→`",
        ));
    } else if parsed.condition.is_empty() || parsed.observable.is_empty() {
        defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L4,
            "both sides of the `→` carry text",
        ));
    }

    if let Some(verb) = claim::and_joined_verb(&parsed.observable) {
        defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L4,
            format!("` and {verb}` joins two verbs; split it into one claim per line"),
        ));
    }

    if parsed.bindings.len() != 1 {
        defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L9,
            format!(
                "every claim carries exactly one binding — `[check: <attr>]`, `[gate: <id>]`, or `[HUMAN-ATTENDED]`; this line carries {}",
                parsed.bindings.len()
            ),
        ));
    }

    match &parsed.believe {
        Believe::Malformed => defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L12,
            "a BELIEVE mark reads `BELIEVE:<path> — ` after the claim id",
        )),
        Believe::Mark(path) => believe_line(context, line, path, tree, defects),
        Believe::Absent => {}
    }

    if matches!(parsed.believe, Believe::Absent)
        && !body.contains("(given)")
        && !body.contains("(GUESS)")
    {
        let numerals = claim::unsourced_numerals(body);
        if !numerals.is_empty() {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L7,
                format!(
                    "unsourced numeral{} {}: source it on a `BELIEVE:<path>` line, or suffix `(given)` or `(GUESS)`",
                    if numerals.len() == 1 { "" } else { "s" },
                    numerals.join(", ")
                ),
            ));
        }
    }

    identifiers(context, line, vocabulary, tree, defects);
}

fn believe_line(
    context: &Context,
    line: &Logical,
    path: &str,
    tree: &Tree,
    defects: &mut Vec<Defect>,
) {
    let Some(bytes) = tree.read(path) else {
        defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L12,
            format!("BELIEVE:{path} does not resolve in the tree at this revision"),
        ));
        return;
    };
    let absent: Vec<String> = claim::backticked(&line.text)
        .into_iter()
        .filter(|span| !bytes.contains(span.as_str()))
        .collect();
    if !absent.is_empty() {
        defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L12,
            format!(
                "`{}` {} absent from the believed file {path}",
                absent.join("`, `"),
                if absent.len() == 1 { "is" } else { "are" }
            ),
        ));
    }
}

fn identifiers(
    context: &Context,
    line: &Logical,
    vocabulary: &Vocabulary,
    tree: &Tree,
    defects: &mut Vec<Defect>,
) {
    let mut absent: Vec<String> = Vec::new();
    for span in claim::backticked(&line.text) {
        for token in tree::identifier_tokens(&span) {
            let in_context = tree.exists(&token)
                || vocabulary.carries(&token)
                || tree.believed_bytes_carry(&token);
            if !in_context && !absent.contains(&token) {
                absent.push(token);
            }
        }
    }
    if !absent.is_empty() {
        defects.push(Defect::blocking(
            &context.file,
            line.number,
            RuleId::L8,
            format!(
                "`{}` {} absent from the tree, the Vocabulary section, and the `(NEW)` set",
                absent.join("`, `"),
                if absent.len() == 1 { "is" } else { "are" }
            ),
        ));
    }
}

fn claims(
    document: &Document,
    context: &Context,
    bodies: &Bodies,
    vocabulary: &Vocabulary,
    tree: &Tree,
    defects: &mut Vec<Defect>,
) {
    static HEADING: OnceLock<Regex> = OnceLock::new();
    let heading = HEADING.get_or_init(|| Regex::new(r"^### R([0-9]+) — (.+)$").expect("compiles"));

    let Some(section) = document.section("Claims") else {
        return;
    };
    if bodies.get("Claims") != Some(&Body::Content) {
        return;
    }

    let mut group: Option<u32> = None;
    let mut groups: Vec<u32> = Vec::new();
    let mut why_seen = false;
    let mut last_index: Option<u32> = None;
    let mut ids: BTreeSet<(u32, u32)> = BTreeSet::new();

    for line in content_lines(document, section) {
        let text = line.trimmed();
        if text.starts_with("###") {
            why_seen = false;
            last_index = None;
            let Some(captured) = heading.captures(text) else {
                defects.push(Defect::blocking(
                    &context.file,
                    line.number,
                    RuleId::L3,
                    "a claim group heading reads `### R<n> — <name>`; the anchor derives from the number",
                ));
                group = None;
                continue;
            };
            let number: u32 = captured[1].parse().unwrap_or_default();
            if groups.contains(&number) {
                defects.push(Defect::blocking(
                    &context.file,
                    line.number,
                    RuleId::L3,
                    format!("claim group `R{number}` is declared twice"),
                ));
            } else if groups.last().is_some_and(|previous| *previous > number) {
                defects.push(Defect::blocking(
                    &context.file,
                    line.number,
                    RuleId::L3,
                    format!("claim group `R{number}` breaks the ascending group order"),
                ));
            }
            groups.push(number);
            group = Some(number);
            continue;
        }

        let Some(number) = group else {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L1,
                "the Claims section opens with a `### R<n> — <name>` group heading",
            ));
            continue;
        };

        let parsed = claim::claim_id(text);
        if !why_seen {
            why_seen = true;
            if parsed.is_none() {
                continue;
            }
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L1,
                format!("claim group `R{number}` opens with one plain-prose why line"),
            ));
        }

        let Some((claim_group, index, body)) = parsed else {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L4,
                "a claim group body carries its why line and claim lines `<g>.<m> ...` only",
            ));
            continue;
        };

        if claim_group != number {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("claim `{claim_group}.{index}` sits under group `R{number}`"),
            ));
        }
        if last_index.is_some_and(|previous| index <= previous) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!(
                    "claim `{claim_group}.{index}` breaks the ascending order within its group"
                ),
            ));
        }
        last_index = Some(index);
        if !ids.insert((claim_group, index)) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("claim id `{claim_group}.{index}` is not unique"),
            ));
        }

        arrow_line(context, &line, body, vocabulary, tree, defects);
    }
}

fn unchanged(
    document: &Document,
    context: &Context,
    bodies: &Bodies,
    vocabulary: &Vocabulary,
    tree: &Tree,
    defects: &mut Vec<Defect>,
) {
    let Some(section) = document.section("Unchanged") else {
        return;
    };
    if bodies.get("Unchanged") != Some(&Body::Content) {
        return;
    }

    let mut last: Option<u32> = None;
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    for line in content_lines(document, section) {
        let Some((index, body)) = claim::dotted_id(line.trimmed(), 'U') else {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L4,
                "an Unchanged line reads `U.<m> <condition> → <observable> [binding]`",
            ));
            continue;
        };
        if last.is_some_and(|previous| index <= previous) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("`U.{index}` breaks the ascending order of the Unchanged section"),
            ));
        }
        last = Some(index);
        if !seen.insert(index) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("`U.{index}` is not unique"),
            ));
        }
        arrow_line(context, &line, body, vocabulary, tree, defects);
    }
}

fn unknowns(document: &Document, context: &Context, bodies: &Bodies, defects: &mut Vec<Defect>) {
    static UNKNOWN: OnceLock<Regex> = OnceLock::new();
    static DECISION: OnceLock<Regex> = OnceLock::new();
    let unknown = UNKNOWN
        .get_or_init(|| Regex::new(r"^UNKNOWN-[0-9]+ (\[BLOCKING\] )?.+ — .+$").expect("compiles"));
    let decision = DECISION.get_or_init(|| {
        Regex::new(r"^DECISION-[0-9]+ .+\? proposed: .+ \((GUESS|given)\)$").expect("compiles")
    });

    let Some(section) = document.section("Unknowns") else {
        return;
    };
    if bodies.get("Unknowns") != Some(&Body::Content) {
        return;
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for line in content_lines(document, section) {
        let text = line.trimmed().trim();
        if !unknown.is_match(text) && !decision.is_match(text) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L10,
                "the Unknowns section admits `UNKNOWN-<n> [BLOCKING]? <what> — <action>` and `DECISION-<n> <question>? proposed: <answer> (GUESS|given)` only",
            ));
            continue;
        }
        let id = text
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        if !seen.insert(id.clone()) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("`{id}` is not unique"),
            ));
        }
    }
}

fn stages(document: &Document, context: &Context, bodies: &Bodies, defects: &mut Vec<Defect>) {
    static HEADING: OnceLock<Regex> = OnceLock::new();
    let heading = HEADING.get_or_init(|| Regex::new(r"^### S([0-9]+) — (.+)$").expect("compiles"));

    let Some(section) = document.section("Stages") else {
        return;
    };
    if bodies.get("Stages") != Some(&Body::Content) {
        return;
    }

    let mut seen: Vec<u32> = Vec::new();
    for line in content_lines(document, section) {
        let text = line.trimmed();
        if !text.starts_with("###") {
            continue;
        }
        let Some(captured) = heading.captures(text) else {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                "a stage heading reads `### S<n> — <name>`; the anchor derives from the number",
            ));
            continue;
        };
        let number: u32 = captured[1].parse().unwrap_or_default();
        if seen.contains(&number) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("stage `S{number}` is declared twice"),
            ));
        } else if seen.last().is_some_and(|previous| *previous > number) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("stage `S{number}` breaks the ascending stage order"),
            ));
        }
        seen.push(number);
    }
}

fn forbidden(
    document: &Document,
    context: &Context,
    bodies: &Bodies,
    vocabulary: &Vocabulary,
    tree: &Tree,
    defects: &mut Vec<Defect>,
) {
    let Some(section) = document.section("Forbidden") else {
        return;
    };
    if bodies.get("Forbidden") != Some(&Body::Content) {
        return;
    }

    let mut last: Option<u32> = None;
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    for line in content_lines(document, section) {
        let Some((index, body)) = claim::dotted_id(line.trimmed(), 'F') else {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L4,
                "a Forbidden line reads `F.<m> Do not <...>` or `F.<m> Never <...>`",
            ));
            continue;
        };
        if last.is_some_and(|previous| index <= previous) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("`F.{index}` breaks the ascending order of the Forbidden section"),
            ));
        }
        last = Some(index);
        if !seen.insert(index) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L3,
                format!("`F.{index}` is not unique"),
            ));
        }
        if !body.starts_with("Do not ") && !body.starts_with("Never ") {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L4,
                "a prohibition is verb-first: `Do not <...>` or `Never <...>`",
            ));
        }
        if let Some(verb) = claim::and_joined_verb(body) {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L4,
                format!("` and {verb}` joins two verbs; one prohibition per line"),
            ));
        }
        identifiers(context, &line, vocabulary, tree, defects);
    }
}

fn doubt(document: &Document, context: &Context, ratified: bool, defects: &mut Vec<Defect>) {
    static BLOCKING_UNKNOWN: OnceLock<Regex> = OnceLock::new();
    let blocking_unknown = BLOCKING_UNKNOWN
        .get_or_init(|| Regex::new(r"^UNKNOWN-[0-9]+ \[BLOCKING\]").expect("compiles"));

    for line in &document.lines {
        if line.fenced || line.is_blank() {
            continue;
        }
        let text = line.trimmed().trim();
        let doubt = if text.contains("(GUESS)") {
            "a `(GUESS)` is outstanding"
        } else if blocking_unknown.is_match(text) {
            "a `[BLOCKING]` unknown is outstanding"
        } else {
            continue;
        };
        let message = format!(
            "{doubt}; doubt is resolved by a typed operator answer before `Status: ratified`"
        );
        defects.push(if ratified {
            Defect::blocking(&context.file, line.number, RuleId::L10, message)
        } else {
            Defect::warning(&context.file, line.number, RuleId::L10, message)
        });
    }
}

fn lexical(document: &Document, context: &Context, defects: &mut Vec<Defect>) {
    const BLOCKING_SECTIONS: [&str; 3] = ["Claims", "Unchanged", "Forbidden"];

    for line in &document.lines {
        if line.fenced || line.is_blank() {
            continue;
        }
        let prose = claim::without_backticks(line.trimmed());
        let section = document
            .section_of(line.number)
            .map(|section| section.name.as_str());
        let blocking = section.is_some_and(|name| BLOCKING_SECTIONS.contains(&name));

        let hedges = lexicon::hedges(&prose);
        if !hedges.is_empty() {
            let message = format!(
                "hedge {}; a hedge is a decision that was dodged",
                hedges
                    .iter()
                    .map(|word| format!("`{word}`"))
                    .collect::<Vec<String>>()
                    .join(", ")
            );
            defects.push(if blocking {
                Defect::blocking(&context.file, line.number, RuleId::L5, message)
            } else {
                Defect::warning(&context.file, line.number, RuleId::L5, message)
            });
        }

        let markers = lexicon::open_set_markers(&prose);
        if !markers.is_empty() {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L6,
                format!(
                    "{} opens a set the spec has to close",
                    markers
                        .iter()
                        .map(|marker| format!("`{marker}`"))
                        .collect::<Vec<String>>()
                        .join(" and ")
                ),
            ));
        }

        let models = lexicon::model_names(line.trimmed());
        if !models.is_empty() {
            defects.push(Defect::blocking(
                &context.file,
                line.number,
                RuleId::L16,
                format!(
                    "the model name {} is a host-catalog fact, never spec bytes",
                    models
                        .iter()
                        .map(|name| format!("`{name}`"))
                        .collect::<Vec<String>>()
                        .join(", ")
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{lint_text, Context};
    use crate::defect::{Outcome, Severity};
    use crate::rules::RuleId;

    fn context() -> Context {
        Context {
            file: "spec.md".to_owned(),
            identity: "sample".to_owned(),
            directory: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
            root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        }
    }

    fn spec(body: &str) -> String {
        format!(
            "# sample — a sample spec\nStatus: proposed\nGoverns: silent-factory-worklists/sample.json\nConsumers: the unit tests of this module\nSupersedes: none\n\n{body}"
        )
    }

    fn rules(body: &str) -> Vec<RuleId> {
        let mut found: Vec<RuleId> = lint_text(&spec(body), &context())
            .into_iter()
            .map(|defect| defect.rule)
            .collect();
        found.dedup();
        found
    }

    const SECTIONS: &str = "## Outcome\n\nOne. Two. Three.\n\n## Vocabulary\n\nOmitted: none needed.\n\n## Rulings\n\n| id | decision | ruling |\n|---|---|---|\n| S1 | shape | the sample carries one ruling |\n\n## Claims\n\n### R1 — the sample\nWhy: the unit tests need a group.\n1.1 the linter reads this → the run reports nothing (given). [gate: cargo-tests]\n\n## Unchanged\n\nOmitted: nothing holds.\n\n## Unknowns\n\nOmitted: no doubt outstanding.\n\n## Stages\n\nOmitted: not built.\n\n## Forbidden\n\nF.1 Do not extend this sample.\n";

    #[test]
    fn a_well_formed_spec_reports_nothing() {
        let defects = lint_text(&spec(SECTIONS), &context());
        assert!(defects.is_empty(), "unexpected defects: {defects:?}");
        assert_eq!(Outcome::of(&defects), Outcome::Clean);
    }

    #[test]
    fn a_section_after_forbidden_breaks_the_order() {
        let stages = "## Stages\n\nOmitted: not built.\n\n";
        let moved = format!("{}\n{}", SECTIONS.replace(stages, ""), stages.trim_end());
        assert!(rules(&moved).contains(&RuleId::L1));
    }

    #[test]
    fn doubt_warns_at_proposed_and_blocks_at_ratified() {
        let doubtful = SECTIONS.replace(
            "## Unknowns\n\nOmitted: no doubt outstanding.",
            "## Unknowns\n\nDECISION-1 which steward? proposed: narrator (GUESS)",
        );
        let proposed = lint_text(&spec(&doubtful), &context());
        assert_eq!(Outcome::of(&proposed), Outcome::Warnings);
        assert_eq!(proposed[0].rule, RuleId::L10);
        assert_eq!(proposed[0].severity, Severity::Warning);

        let ratified = spec(&doubtful).replace(
            "Status: proposed\nGoverns: silent-factory-worklists/sample.json",
            "Status: ratified 2026-08-17\nGoverns: Cargo.toml",
        );
        let defects = lint_text(&ratified, &context());
        assert!(defects
            .iter()
            .any(|defect| defect.rule == RuleId::L10 && defect.severity == Severity::Blocking));
    }

    #[test]
    fn the_status_block_and_the_section_set_are_both_enforced() {
        assert!(
            rules(&SECTIONS.replace("## Unchanged\n\nOmitted: nothing holds.\n\n", ""))
                .contains(&RuleId::L15)
        );
        assert!(rules(&SECTIONS.replace("## Claims", "## Claimz")).contains(&RuleId::L1));
        let no_consumers =
            spec(SECTIONS).replace("Consumers: the unit tests of this module", "Consumers:");
        assert!(lint_text(&no_consumers, &context())
            .iter()
            .any(|defect| defect.rule == RuleId::L2));
    }
}
