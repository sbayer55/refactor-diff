//! Line and column conventions shared by every analyzer.

use serde::{Deserialize, Serialize};

/// A position in a file: 1-based line, 0-based column counted in Unicode code points.
///
/// Positions order lexicographically, which every range comparison in the engine relies on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub const fn new(line: u32, col: u32) -> Self {
        Self { line, col }
    }
}

/// Split on `\n` the same way line-based tokenizers number lines: a trailing newline does not
/// start an extra line, and a `\r` before the newline is dropped.
pub fn split_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
        .iter()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect()
}

/// Convert a UTF-8 byte offset (as parsers report it) into the code-point offset tokens use.
///
/// Out-of-range lines return the byte offset unchanged; an offset inside a multi-byte
/// character counts that character, matching Python's `decode(errors="replace")`.
pub fn char_col(lines: &[String], line: u32, byte_col: usize) -> u32 {
    let Some(text) = line.checked_sub(1).and_then(|i| lines.get(i as usize)) else {
        return byte_col as u32;
    };
    let mut count = 0u32;
    for (start, ch) in text.char_indices() {
        if start >= byte_col {
            break;
        }
        count += 1;
        if start + ch.len_utf8() > byte_col {
            // A cut inside this character: the partial bytes decode to one replacement char.
            break;
        }
    }
    if byte_col > text.len() {
        // Bytes past the end of the line never exist in the source; Python slicing clamps.
        return text.chars().count() as u32;
    }
    count
}

/// Number of code points in `s`.
pub fn char_len(s: &str) -> u32 {
    s.chars().count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_lines_drops_trailing_newline_and_cr() {
        assert_eq!(split_lines("a\r\nb\n"), vec!["a", "b"]);
        assert_eq!(split_lines("a\nb"), vec!["a", "b"]);
        assert_eq!(split_lines(""), Vec::<String>::new());
        assert_eq!(split_lines("\n"), vec![""]);
    }

    #[test]
    fn char_col_counts_code_points() {
        let lines = vec!["const é = \"ü\"; x".to_string()];
        assert_eq!(char_col(&lines, 1, 0), 0);
        assert_eq!(char_col(&lines, 1, 6), 6); // byte 6 is the start of é
        assert_eq!(char_col(&lines, 1, 8), 7); // after é (2 bytes)
        assert_eq!(char_col(&lines, 1, 7), 7); // cut inside é counts it
        assert_eq!(char_col(&lines, 2, 5), 5); // unknown line: unchanged
    }

    #[test]
    fn pos_orders_by_line_then_col() {
        assert!(Pos::new(1, 10) < Pos::new(2, 0));
        assert!(Pos::new(2, 0) < Pos::new(2, 1));
    }
}
