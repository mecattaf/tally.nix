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
    supersession_details, SupersessionDetails, SUPERSESSION_CODES, SUPERSESSION_DETAIL_FIELDS,
};

const FLOW_DOC: &str = include_str!("../../../doc/src/flows/submission-and-replay.md");
const ERROR_DOC: &str = include_str!("../../../doc/src/reference/errors.md");

/// The text between one `<!-- name:start -->` / `<!-- name:end -->` pair.
fn marked<'a>(doc: &'a str, name: &str) -> &'a str {
    let (_, after_start) = doc
        .split_once(&format!("<!-- {name}:start -->"))
        .unwrap_or_else(|| panic!("documentation must contain the {name} start marker"));
    let (marked, _) = after_start
        .split_once(&format!("<!-- {name}:end -->"))
        .unwrap_or_else(|| panic!("documentation must contain the {name} end marker"));
    marked
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
fn marked_table<'a>(doc: &'a str, name: &str) -> &'a str {
    let span = marked(doc, name);
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

#[test]
fn the_remedy_nullity_rule_is_stated_once_and_is_true() {
    let flows = marked(FLOW_DOC, "remedy-nullity");
    let errors = marked(ERROR_DOC, "remedy-nullity");
    assert_eq!(
        flows, errors,
        "the two pages must state one rule in one wording"
    );

    // And the wording must be true of the shipped code: a refusal that names no
    // run carries no command, on every member of the family.
    for code in SUPERSESSION_CODES {
        let unnamed = supersession_details(code, &SupersessionDetails::default());
        assert!(
            unnamed["remedy"].is_null(),
            "{code}: a refusal naming no run must advertise no command"
        );
    }
    assert!(
        !fully_populated("script-changed-mid-run")["remedy"].is_null(),
        "a named run still gets its command"
    );
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
