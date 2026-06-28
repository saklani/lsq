//! Typed source positions — the compile-time fix for the footgun we hit by hand.
//!
//! There are TWO coordinate systems and confusing them silently corrupts every query:
//!
//!   * `UserPos` — what the agent/CLI speaks: 1-based line, 1-based *character* column
//!                 (counted in Unicode scalar values, i.e. `char`s).
//!   * `LspPos`  — what the protocol speaks: 0-based line, 0-based offset measured in
//!                 UTF-16 code units (the LSP default `positionEncoding`).
//!
//! They are DISTINCT TYPES, so you cannot pass one where the other is expected. The only
//! bridge is [`UserPos::to_lsp`], which *requires the text of the line* — because the
//! UTF-16 offset depends on the actual characters (e.g. an emoji is one `char` but two
//! UTF-16 units). That requirement makes the conversion impossible to skip or fake.
//!
//! (Later: tsgo advertises `positionEncoding`, so we may negotiate UTF-8 and swap the
//! counting rule — but the *type boundary* stays exactly the same.)

/// A position as a human/agent types it: 1-based line, 1-based character column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserPos {
    pub line1: u32,
    pub col1: u32,
}

/// A position as LSP wants it: 0-based line, 0-based UTF-16 code-unit offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LspPos {
    pub line0: u32,
    pub char0_utf16: u32,
}

impl UserPos {
    pub fn new(line1: u32, col1: u32) -> Self {
        Self { line1, col1 }
    }

    /// Convert to an LSP position. `line_text` is the full text of the target line
    /// (without the trailing newline). Returns `None` if the column is out of range.
    pub fn to_lsp(self, line_text: &str) -> Option<LspPos> {
        if self.line1 == 0 || self.col1 == 0 {
            return None; // both systems are degenerate at 0 on the user side
        }
        let chars_before = (self.col1 - 1) as usize;
        if chars_before > line_text.chars().count() {
            return None; // column points past end-of-line (+1 past last char is allowed)
        }
        // Sum the UTF-16 length of every character to the LEFT of the cursor.
        let char0_utf16 = line_text
            .chars()
            .take(chars_before)
            .map(|c| c.len_utf16() as u32)
            .sum();
        Some(LspPos {
            line0: self.line1 - 1,
            char0_utf16,
        })
    }
}

impl LspPos {
    /// Convert back to a user-facing position. Needs `line_text` (the target line) to undo
    /// the UTF-16 math — the exact inverse of [`UserPos::to_lsp`].
    pub fn to_user(self, line_text: &str) -> UserPos {
        let mut utf16 = 0u32;
        let mut chars = 0u32;
        for ch in line_text.chars() {
            if utf16 >= self.char0_utf16 {
                break;
            }
            utf16 += ch.len_utf16() as u32;
            chars += 1;
        }
        UserPos { line1: self.line0 + 1, col1: chars + 1 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_is_one_to_one() {
        // `login` on line 7, pointing at the 'l' (col 3 in the auth.ts demo earlier).
        let lsp = UserPos::new(7, 3).to_lsp("  login(user) {}").unwrap();
        assert_eq!(lsp, LspPos { line0: 6, char0_utf16: 2 });
    }

    #[test]
    fn emoji_costs_two_utf16_units() {
        // chars (1-based): l(1) e(2) t(3) ' '(4) a(5) ' '(6) =(7) ' '(8) '(9) 😀(10) '(11) ;(12)
        // The 😀 is ONE char but TWO UTF-16 code units, so the ';' at char-col 12 sits at
        // UTF-16 offset 12 (= 11 chars to its left, one of which counts double).
        let line = "let a = '😀';";
        let lsp = UserPos::new(1, 12).to_lsp(line).unwrap();
        assert_eq!(lsp, LspPos { line0: 0, char0_utf16: 12 });
    }

    #[test]
    fn out_of_range_is_none() {
        assert!(UserPos::new(1, 99).to_lsp("short").is_none());
        assert!(UserPos::new(0, 1).to_lsp("x").is_none());
    }

    #[test]
    fn lsp_to_user_round_trips_through_emoji() {
        let line = "let a = '😀';";
        let user = UserPos::new(1, 12);
        let lsp = user.to_lsp(line).unwrap();
        // back again lands on the same 1-based char column
        assert_eq!(lsp.to_user(line), user);
    }
}
