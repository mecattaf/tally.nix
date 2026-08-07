//! Pin the exit-20 `details` prose to the constant it describes.
//!
//! The fourteen members are stated in four places: `SUPERSESSION_DETAIL_FIELDS`,
//! the table in `doc/src/flows/submission-and-replay.md`, the per-code prose in
//! `doc/src/reference/errors.md`, and the changelog. The in-crate contract test
//! asserts production output against the same constant production iterates over
//! — genuine for site coverage, tautological for membership and ordering: add or
//! reorder a member and every assertion still passes while all three prose
//! copies rot.
//!
//! `crates/tally-core/tests/rpc_docs.rs` closes exactly this gap for the RPC
//! method list by parsing a marker-delimited table out of the reference and
//! comparing it with the code's constant. This is that pattern applied to the
//! supersession family, extended to the derived members whose *values* the docs
//! also state.

use std::collections::BTreeSet;

use serde_json::{Map, Value};
use tally_flow::{
    supersession_details, with_recovery_facts, FlowError, SupersessionDetails, SUPERSESSION_CODES,
    SUPERSESSION_DETAIL_FIELDS,
};

const FLOW_DOC: &str = include_str!("../../../doc/src/flows/submission-and-replay.md");
const ERROR_DOC: &str = include_str!("../../../doc/src/reference/errors.md");

/// The text between one `<!-- name:start -->` / `<!-- name:end -->` pair, and
/// the text that follows the end marker.
fn marked_and_after<'a>(doc: &'a str, name: &str) -> (&'a str, &'a str) {
    let (_, after_start) = doc
        .split_once(&format!("<!-- {name}:start -->"))
        .unwrap_or_else(|| panic!("documentation must contain the {name} start marker"));
    after_start
        .split_once(&format!("<!-- {name}:end -->"))
        .unwrap_or_else(|| panic!("documentation must contain the {name} end marker"))
}

/// The text between one `<!-- name:start -->` / `<!-- name:end -->` pair.
fn marked<'a>(doc: &'a str, name: &str) -> &'a str {
    marked_and_after(doc, name).0
}

/// Every backticked span in a fragment of prose, in order.
fn backticked(text: &str) -> Vec<&str> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect()
}

/// A marked span that must be a whole markdown table, header row and all.
///
/// A marker line dropped *between* two rows ends the table as far as the
/// renderer is concerned: the remaining rows fall out as a paragraph of literal
/// pipes and the next data row is promoted to a second table's header. `mdbook
/// build` reports nothing, because that is all valid markdown. The pin has to
/// carry this itself, so the convention it depends on cannot quietly ship a
/// broken reference page.
///
/// Both ends are checked, because both ends can be wrong and the one that
/// actually shipped was the *end*: a span that starts below the header leaves a
/// bodiless table above it, and a span that ends between two rows leaves the
/// remaining rows as prose below it. Guarding only the start would state an
/// anti-recurrence guarantee this helper does not provide — which is the same
/// gap between a claim and its evidence that these pins exist to close.
fn marked_table<'a>(doc: &'a str, name: &str) -> &'a str {
    let (span, after) = marked_and_after(doc, name);
    if let Some(next) = after.lines().find(|line| !line.trim().is_empty()) {
        assert!(
            !next.trim_start().starts_with('|'),
            "{name} must wrap the whole table: the end marker is followed by {next:?}, \
             another table row — a marker inside a table splits it in the rendered book"
        );
    }
    let mut lines = span.lines().filter(|line| !line.trim().is_empty());
    let header = lines
        .next()
        .unwrap_or_else(|| panic!("{name} marks no table at all"));
    let separator = lines
        .next()
        .unwrap_or_else(|| panic!("{name} marks a header with no rows"));
    assert!(
        header.trim_start().starts_with('|'),
        "{name} must open on the table's header row, not on {header:?} — a marker \
         inside a table splits it in the rendered book"
    );
    assert!(
        separator
            .trim()
            .trim_matches('|')
            .split('|')
            .all(|cell| !cell.trim().is_empty()
                && cell
                    .trim()
                    .chars()
                    .all(|glyph| glyph == '-' || glyph == ':')),
        "{name} must wrap the whole table: the second line is {separator:?}, not a \
         header separator"
    );
    span
}

/// The cells of one markdown table row, header and separator rows excluded.
fn table_rows(table: &str) -> Vec<Vec<&str>> {
    table
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .map(|line| {
            line.trim()
                .trim_start_matches('|')
                .trim_end_matches('|')
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>()
        })
        .filter(|cells| {
            // A header cell is bare prose and a separator cell is dashes; every
            // row this pin cares about opens with a backticked identifier.
            cells
                .first()
                .is_some_and(|first| first.starts_with('`') && first.ends_with('`'))
        })
        .collect()
}

/// The `details` map a fully-informed raising site produces with one member
/// withheld, completed through the production path.
///
/// This is how the nullity rule's *operand* becomes load-bearing: the member the
/// sentence blames is read out of the sentence and withheld here, so a rule that
/// blames the wrong member fails rather than passing on a coincidence.
fn completed_without(code: &str, member: &str) -> Map<String, Value> {
    let mut error = FlowError::new("FlowReplayError", code, "probe");
    error.details = fully_populated(code);
    assert!(
        error.details.remove(member).is_some(),
        "{member} is not a member of the completed map"
    );
    with_recovery_facts(error).details
}

/// The `details` map a raising site that knows everything would produce.
fn fully_populated(code: &str) -> Map<String, Value> {
    supersession_details(
        code,
        &SupersessionDetails {
            flow_run_id: "00000000-0000-4000-8000-00000000000d",
            recorded_hash: Some("sha256:recorded"),
            current_hash: Some("sha256:current"),
            recorded_label: Some("recorded-label"),
            current_label: Some("current-label"),
            task_uuid: Some("00000000-0000-4000-8000-00000000000e"),
            successor_flow_run_id: Some("00000000-0000-4000-8000-00000000000f"),
            reason: Some("script-changed"),
            recorded_at: Some("2026-08-06T00:00:00Z"),
            kernel_error: Some("dedup-key-conflict"),
        },
    )
}

#[test]
fn the_documented_detail_table_is_the_constant() {
    let documented = table_rows(marked_table(FLOW_DOC, "supersession-detail-fields"))
        .into_iter()
        .map(|cells| cells[0].trim_matches('`'))
        .collect::<Vec<_>>();
    assert_eq!(documented, SUPERSESSION_DETAIL_FIELDS);
}

#[test]
fn the_documented_family_list_is_the_constant() {
    assert_eq!(
        backticked(marked(FLOW_DOC, "supersession-codes")),
        SUPERSESSION_CODES
    );
}

#[test]
fn the_error_reference_tabulates_the_family_in_the_same_order() {
    let documented = table_rows(marked_table(ERROR_DOC, "supersession-code-rows"))
        .into_iter()
        .map(|cells| cells[0].trim_matches('`'))
        .collect::<Vec<_>>();
    assert_eq!(documented, SUPERSESSION_CODES);
}

#[test]
fn the_error_reference_populates_column_names_only_contract_members() {
    let contract = SUPERSESSION_DETAIL_FIELDS
        .into_iter()
        .collect::<BTreeSet<_>>();
    for row in table_rows(marked_table(ERROR_DOC, "supersession-code-rows")) {
        let code = row[0].trim_matches('`');
        for token in backticked(row[3]) {
            // `divergentInput: "script"` names a member and its value.
            let member = token.split(':').next().unwrap_or(token).trim();
            assert!(
                contract.contains(member),
                "{code}: the Populates column names {member:?}, which is not a contract member"
            );
        }
    }
}

#[test]
fn the_error_reference_states_the_derived_members_the_code_computes() {
    for row in table_rows(marked_table(ERROR_DOC, "supersession-code-rows")) {
        let code = row[0].trim_matches('`');
        let details = fully_populated(code);
        let populates = backticked(row[3]);

        // Columns two and three are `transient` and `resolution` verbatim.
        assert_eq!(
            row[1].trim_matches('`'),
            details["transient"].to_string(),
            "{code}: documented transient"
        );
        assert_eq!(
            row[2].trim_matches('`'),
            details["resolution"]
                .as_str()
                .expect("resolution is a string"),
            "{code}: documented resolution"
        );

        // `divergentInput` is written into the Populates column as
        // `divergentInput: "<value>"`, or omitted when the code has none.
        let documented_input = populates.iter().find_map(|token| {
            token
                .strip_prefix("divergentInput:")
                .map(|value| value.trim().trim_matches('"').to_owned())
        });
        assert_eq!(
            documented_input.as_deref(),
            details["divergentInput"].as_str(),
            "{code}: documented divergentInput"
        );

        // `remedy` is listed exactly for the codes one command clears.
        assert_eq!(
            populates.contains(&"remedy"),
            !details["remedy"].is_null(),
            "{code}: documented remedy"
        );
    }
}

/// The `remedy` nullity rule is stated once, and the code check is driven by
/// what that sentence says rather than run beside it.
///
/// The rule names two things: the value `remedy` takes, and the member whose
/// absence produces it. Both are read out of the marked span and used to drive
/// the code check, so the sentence is load-bearing: a span that states a
/// different value, blames a different member, or states nothing at all fails
/// here. An earlier version of this test asserted only that the two spans were
/// identical *to each other* and, separately, that the code returns `null` —
/// two claims sharing a name and nothing else, under which both public pages
/// could be emptied or inverted in lockstep and the test still passed.
///
/// What it still cannot do is read English: a sentence built from the right
/// operands but negated ("`null` is never returned when no `flowRunId` is
/// known") would pass. It binds the rule's operands and its polarity, not its
/// grammar, and it says so rather than claiming to check the prose.
#[test]
fn the_remedy_nullity_rule_is_stated_once_and_the_code_obeys_the_stated_rule() {
    let flows = marked(FLOW_DOC, "remedy-nullity");
    let errors = marked(ERROR_DOC, "remedy-nullity");
    assert_eq!(
        flows, errors,
        "the two pages must state one rule in one wording"
    );

    // What the code does, computed before the sentence is read, so the sentence
    // is checked against behaviour rather than against itself.
    let unnamed = SUPERSESSION_CODES
        .iter()
        .map(|code| {
            supersession_details(code, &SupersessionDetails::default())["remedy"].to_string()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        unnamed,
        BTreeSet::from(["null".to_owned()]),
        "a refusal naming no run must advertise no command, on every code"
    );
    let value = unnamed.into_iter().next().expect("one distinct value");

    // The rule must open on the value the code produces. An emptied span has no
    // value to open on, and a wording like "always present and always a command"
    // is describing something other than a `null`.
    let stated = backticked(flows);
    assert_eq!(
        stated.first().copied(),
        Some(value.as_str()),
        "the rule must open on the value the code produces (`{value}`), not on {stated:?}"
    );

    // And it must blame a real contract member — the one whose absence actually
    // produces that value. Withholding the member the sentence names has to
    // reproduce it, and a fully-informed refusal has to not.
    let operand = stated
        .iter()
        .find(|token| SUPERSESSION_DETAIL_FIELDS.contains(token))
        .unwrap_or_else(|| {
            panic!(
                "the rule must name the contract member whose absence yields `{value}`: {stated:?}"
            )
        });
    assert_eq!(
        completed_without("script-changed-mid-run", operand)["remedy"].to_string(),
        value,
        "the rule blames {operand}, but withholding it does not yield `{value}`"
    );
    assert_ne!(
        fully_populated("script-changed-mid-run")["remedy"].to_string(),
        value,
        "a refusal that supplies {operand} still gets its command"
    );
}

/// Issue #414: the stated rule has a second half now — a `flowRunId` that
/// reads as a command flag also yields `null` — and the same discipline
/// applies to it: the sentence is checked against what the code does, not
/// against itself. A rule that names only the missing-id half while the code
/// suppresses two shapes is the defect this file exists to catch, one level
/// up.
#[test]
fn the_stated_rule_covers_the_flag_shaped_identity_the_code_also_suppresses() {
    let flag_shaped = supersession_details(
        "script-changed-mid-run",
        &SupersessionDetails {
            flow_run_id: "--reason",
            ..SupersessionDetails::default()
        },
    );
    assert!(
        flag_shaped["remedy"].is_null(),
        "a flag-shaped identity must advertise no command: {}",
        flag_shaped["remedy"]
    );
    assert_eq!(
        flag_shaped["flowRunId"], "--reason",
        "and the badly named run stays visible"
    );

    for (page, doc) in [
        ("submission-and-replay.md", FLOW_DOC),
        ("errors.md", ERROR_DOC),
    ] {
        let rule = marked(doc, "remedy-nullity");
        assert!(
            rule.contains("command flag"),
            "{page} states only the missing-id half of a rule the code applies to two \
             shapes: {rule:?}"
        );
    }
}

#[test]
fn both_pages_state_the_size_of_the_contract() {
    let size = match SUPERSESSION_DETAIL_FIELDS.len() {
        13 => "thirteen",
        14 => "fourteen",
        15 => "fifteen",
        other => panic!("spell {other} out here and in both pages"),
    };
    let phrase = format!("same {size} ");
    assert!(
        FLOW_DOC.contains(&phrase),
        "submission-and-replay.md must say {phrase:?}"
    );
    assert!(ERROR_DOC.contains(&phrase), "errors.md must say {phrase:?}");
}
