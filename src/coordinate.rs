//! Coordinate translation between the two position vocabularies — the single place that
//! bridges [`agent::Position`](crate::agent::Position) (1-based, character columns) and
//! [`protocol::Position`](crate::protocol::Position) (0-based, UTF-16 code units).
//!
//! Kept OFF both data types so `agent` and `protocol` stay pure, independent leaves and the
//! encoding math lives in one named layer. Both directions REQUIRE the target line's text,
//! because the UTF-16 offset depends on the actual characters (an emoji is one `char` but two
//! UTF-16 units) — a requirement that makes the conversion impossible to skip or fake.

use crate::{agent, protocol};

/// Agent → wire. `line_text` is the full target line (no trailing newline). Returns `None`
/// if the column is out of range (past end-of-line + 1 is allowed).
pub fn to_protocol(pos: agent::Position, line_text: &str) -> Option<protocol::Position> {
    if pos.line1 == 0 || pos.col1 == 0 {
        return None; // both systems are degenerate at 0 on the agent side
    }
    let chars_before = (pos.col1 - 1) as usize;
    if chars_before > line_text.chars().count() {
        return None;
    }
    // Sum the UTF-16 length of every character to the LEFT of the cursor.
    let char0_utf16 = line_text
        .chars()
        .take(chars_before)
        .map(|c| c.len_utf16() as u32)
        .sum();
    Some(protocol::Position {
        line0: pos.line1 - 1,
        char0_utf16,
    })
}

/// Wire → agent. The exact inverse of [`to_protocol`]; `line_text` undoes the UTF-16 math.
pub fn to_agent(pos: protocol::Position, line_text: &str) -> agent::Position {
    let mut utf16 = 0u32;
    let mut chars = 0u32;
    for ch in line_text.chars() {
        if utf16 >= pos.char0_utf16 {
            break;
        }
        utf16 += ch.len_utf16() as u32;
        chars += 1;
    }
    agent::Position {
        line1: pos.line0 + 1,
        col1: chars + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_to_one() {
        // `login` on line 7, pointing at the 'l' (col 3 in the auth.ts demo earlier).
        let lsp = to_protocol(agent::Position::new(7, 3), "  login(user) {}").unwrap();
        assert_eq!(
            lsp,
            protocol::Position {
                line0: 6,
                char0_utf16: 2
            }
        );
    }

    #[test]
    fn emoji_costs_two_utf16_units() {
        // chars (1-based): l(1) e(2) t(3) ' '(4) a(5) ' '(6) =(7) ' '(8) '(9) 😀(10) '(11) ;(12)
        // The 😀 is ONE char but TWO UTF-16 code units, so the ';' at char-col 12 sits at
        // UTF-16 offset 12 (= 11 chars to its left, one of which counts double).
        let line = "let a = '😀';";
        let lsp = to_protocol(agent::Position::new(1, 12), line).unwrap();
        assert_eq!(
            lsp,
            protocol::Position {
                line0: 0,
                char0_utf16: 12
            }
        );
    }

    #[test]
    fn out_of_range_is_none() {
        assert!(to_protocol(agent::Position::new(1, 99), "short").is_none());
        assert!(to_protocol(agent::Position::new(0, 1), "x").is_none());
    }

    #[test]
    fn round_trips_through_emoji() {
        let line = "let a = '😀';";
        let start = agent::Position::new(1, 12);
        let lsp = to_protocol(start, line).unwrap();
        assert_eq!(to_agent(lsp, line), start);
    }
}
