//! The line grammar. `specs/README.md` §3 is explicit that a spec is parsed as
//! lines and never as a markdown AST, so this module splits a `spec.md` into a
//! preamble and its `##` sections and nothing else.

use std::ops::Range;

/// The section set, in the exact order `specs/README.md` §3 fixes.
pub const SECTION_ORDER: [&str; 8] = [
    "Outcome",
    "Vocabulary",
    "Rulings",
    "Claims",
    "Unchanged",
    "Unknowns",
    "Stages",
    "Forbidden",
];

/// The two sections that may never be omitted.
pub const NEVER_OMITTABLE: [&str; 2] = ["Outcome", "Claims"];

/// One line of a `spec.md`, numbered from 1.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    pub number: usize,
    pub text: String,
    /// Inside a fenced block, including the fence lines themselves. Lexical
    /// rules skip these; a fenced example is a quotation, not a claim.
    pub fenced: bool,
}

impl Line {
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    pub fn trimmed(&self) -> &str {
        self.text.trim_end()
    }
}

/// A `## <name>` section and the lines under it.
#[derive(Clone, Debug)]
pub struct Section {
    pub name: String,
    /// Line number of the `## ` heading itself.
    pub heading: usize,
    /// Indices into [`Document::lines`].
    pub body: Range<usize>,
}

/// A parsed `spec.md`.
#[derive(Clone, Debug)]
pub struct Document {
    pub lines: Vec<Line>,
    preamble: Range<usize>,
    pub sections: Vec<Section>,
}

impl Document {
    pub fn parse(text: &str) -> Self {
        let mut lines = Vec::new();
        let mut fenced = false;
        for (index, raw) in text.lines().enumerate() {
            let fence = raw.trim_start().starts_with("```");
            let inside = fenced || fence;
            if fence {
                fenced = !fenced;
            }
            lines.push(Line {
                number: index + 1,
                text: raw.to_owned(),
                fenced: inside,
            });
        }

        let mut sections: Vec<Section> = Vec::new();
        let mut preamble_end = lines.len();
        for index in 0..lines.len() {
            if lines[index].fenced {
                continue;
            }
            let Some(name) = lines[index].trimmed().strip_prefix("## ") else {
                continue;
            };
            if sections.is_empty() {
                preamble_end = index;
            } else {
                let last = sections.len() - 1;
                sections[last].body.end = index;
            }
            sections.push(Section {
                name: name.trim().to_owned(),
                heading: lines[index].number,
                body: index + 1..lines.len(),
            });
        }

        Self {
            lines,
            preamble: 0..preamble_end,
            sections,
        }
    }

    pub fn preamble(&self) -> &[Line] {
        &self.lines[self.preamble.clone()]
    }

    pub fn section(&self, name: &str) -> Option<&Section> {
        self.sections.iter().find(|section| section.name == name)
    }

    pub fn body(&self, section: &Section) -> &[Line] {
        &self.lines[section.body.clone()]
    }

    /// The section a line number falls in, or `None` for the preamble.
    pub fn section_of(&self, line: usize) -> Option<&Section> {
        self.sections
            .iter()
            .rfind(|section| section.heading <= line)
    }

    /// One section body as logical lines.
    pub fn logical(&self, section: &Section) -> Vec<Logical> {
        let mut logical: Vec<Logical> = Vec::new();
        for line in self.body(section) {
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
}

/// One logical line of a section body: a line plus the indented continuation
/// lines that wrap it. The grammar is line-oriented; wrapping a long line at
/// the margin is typography, not a second line.
#[derive(Clone, Debug)]
pub struct Logical {
    pub number: usize,
    pub text: String,
}

impl Logical {
    pub fn trimmed(&self) -> &str {
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::Document;

    const SAMPLE: &str =
        "# id — title\nStatus: proposed\n\n## Outcome\n\nOne line.\n\n## Claims\n\n### R1 — a\n";

    #[test]
    fn the_preamble_stops_at_the_first_section() {
        let document = Document::parse(SAMPLE);
        let preamble: Vec<&str> = document
            .preamble()
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(preamble, ["# id — title", "Status: proposed", ""]);
    }

    #[test]
    fn sections_carry_their_bodies_and_line_numbers() {
        let document = Document::parse(SAMPLE);
        let names: Vec<&str> = document
            .sections
            .iter()
            .map(|section| section.name.as_str())
            .collect();
        assert_eq!(names, ["Outcome", "Claims"]);

        let outcome = document.section("Outcome").expect("Outcome parses");
        assert_eq!(outcome.heading, 4);
        let body: Vec<&str> = document
            .body(outcome)
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(body, ["", "One line.", ""]);
        assert_eq!(
            document.section_of(6).map(|s| s.name.as_str()),
            Some("Outcome")
        );
        assert_eq!(document.section_of(2).map(|s| s.name.as_str()), None);
    }

    #[test]
    fn a_heading_inside_a_fence_is_not_a_section() {
        let document = Document::parse("# id — title\n\n```\n## Outcome\n```\n\n## Claims\n");
        let names: Vec<&str> = document
            .sections
            .iter()
            .map(|section| section.name.as_str())
            .collect();
        assert_eq!(names, ["Claims"]);
        assert!(document.lines[3].fenced);
    }
}
