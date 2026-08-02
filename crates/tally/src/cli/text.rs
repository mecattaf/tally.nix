//! Terminal-safe rendering of adapter-controlled text.
//!
//! Every human-readable surface prints strings an adapter chose: stderr tails,
//! node labels, task titles, capture paths. Those strings reach an operator's
//! TTY verbatim unless they are filtered here, so a failing job could clear the
//! screen, relocate the cursor, or reorder a line with a bidirectional
//! override. Escape sequences, C0/C1 controls, and bidi format characters carry
//! no information a reader loses by dropping them.

/// Strip terminal control from `value` and collapse all whitespace runs into
/// single spaces. For table cells and single-line fields, where an embedded
/// newline would break the column layout.
pub(super) fn compact_text(value: &str) -> String {
    sanitize_terminal_text(value)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Strip terminal control from one line while preserving its indentation.
/// Stack traces, diffs, and structured logs carry meaning in leading
/// whitespace, so failure tails use this rather than [`compact_text`].
pub(super) fn sanitize_line(value: &str) -> String {
    sanitize_terminal_text(value)
        .trim_end_matches([' ', '\t', '\r'])
        .to_owned()
}

/// Remove escape sequences, C0/C1 controls, and bidirectional format
/// characters. Tab and newline survive; every other control is dropped along
/// with the sequence it introduces.
fn sanitize_terminal_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => skip_escape_sequence(&mut chars),
            '\t' | '\n' => out.push(ch),
            // C0, DEL, and the C1 block, which some terminals decode as
            // single-byte equivalents of the ESC-introduced sequences above.
            ch if (ch < ' ' || ('\u{7f}'..='\u{9f}').contains(&ch)) => {}
            ch if is_bidi_control(ch) => {}
            ch => out.push(ch),
        }
    }
    out
}

/// Consume the remainder of an escape sequence whose ESC was already taken.
/// Recognises CSI and the string-argument introducers (OSC/DCS/SOS/PM/APC);
/// any other introducer is a two-character sequence.
fn skip_escape_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let Some(introducer) = chars.next() else {
        return;
    };
    match introducer {
        '[' => {
            // CSI: parameter and intermediate bytes, then one final byte. A C0
            // control inside the sequence aborts it, as it does on a real
            // terminal: without the bail-out an adapter line that happens to
            // contain a bare `ESC [` swallows everything up to the next
            // 0x40-0x7e byte, which is usually legitimate text.
            while let Some(&ch) = chars.peek() {
                if ch < ' ' || ch == '\u{7f}' {
                    return;
                }
                chars.next();
                if ('\u{40}'..='\u{7e}').contains(&ch) {
                    break;
                }
            }
        }
        ']' | 'P' | 'X' | '^' | '_' => {
            // String sequences run until BEL or a String Terminator (ESC \).
            while let Some(ch) = chars.next() {
                if ch == '\u{7}' {
                    break;
                }
                if ch == '\u{1b}' {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    break;
                }
            }
        }
        _ => {}
    }
}

const fn is_bidi_control(ch: char) -> bool {
    matches!(
        ch,
        // ARABIC LETTER MARK is an implicit directional mark like LRM/RLM and
        // reorders a line the same way; it sits outside the general-punctuation
        // block, which is the only reason it is listed apart.
        '\u{61c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csi_sequences_do_not_reach_the_terminal() {
        assert_eq!(compact_text("\u{1b}[2Jgate failed"), "gate failed");
        assert_eq!(compact_text("red \u{1b}[31mtext\u{1b}[0m"), "red text");
    }

    #[test]
    fn osc_titles_are_consumed_through_their_terminator() {
        assert_eq!(compact_text("\u{1b}]0;pwned\u{7}ok"), "ok");
        assert_eq!(compact_text("\u{1b}]8;;http://x\u{1b}\\link"), "link");
    }

    #[test]
    fn bare_controls_and_bidi_overrides_are_dropped() {
        assert_eq!(compact_text("a\u{7}b\u{8}c"), "abc");
        assert_eq!(compact_text("\u{202e}drowssap"), "drowssap");
        assert_eq!(compact_text("c1\u{9b}2Jtail"), "c12Jtail");
        // U+061C ARABIC LETTER MARK reorders a line like LRM and RLM do.
        assert_eq!(compact_text("\u{61c}x"), "x");
        assert_eq!(compact_text("left\u{61c}right"), "leftright");
    }

    #[test]
    fn an_unterminated_csi_stops_at_a_control_instead_of_eating_the_line() {
        // The scan ends at the newline rather than running on to the first
        // 0x40-0x7e byte, which would have swallowed "g" of "gate".
        assert_eq!(compact_text("\u{1b}[3\ngate failed"), "gate failed");
        assert_eq!(sanitize_line("\u{1b}[3"), "");
        // A well-formed sequence is still consumed whole.
        assert_eq!(compact_text("\u{1b}[3mgate failed"), "gate failed");
    }

    #[test]
    fn compact_text_still_folds_whitespace() {
        assert_eq!(compact_text("  two\n\tlines  "), "two lines");
    }

    #[test]
    fn sanitize_line_keeps_indentation() {
        assert_eq!(
            sanitize_line("    at foo\u{1b}[0m (bar.rs:1)   "),
            "    at foo (bar.rs:1)"
        );
        assert_eq!(sanitize_line("\tdeep"), "\tdeep");
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(sanitize_line("ordinary output"), "ordinary output");
        assert_eq!(compact_text("ordinary output"), "ordinary output");
    }
}
