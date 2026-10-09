//! Line-level diffing: split a file change into hunks and candidate change units.

use crate::seqmatch::{Opcode, SequenceMatcher, Tag};

/// Context lines around each hunk, as in a unified diff.
pub const CONTEXT: usize = 3;

/// The line matcher shared by the engine and the file viewer. Blank lines are junk: they may
/// extend a match but never anchor one, so an import block followed by a blank line is not
/// torn apart to pair the blank line with another.
pub fn line_matcher<'a>(old: &'a [String], new: &'a [String]) -> SequenceMatcher<'a, String> {
    SequenceMatcher::with_junk(old, new, |line| line.trim().is_empty())
}

/// Unified-diff style hunk groups (opcodes including surrounding context).
pub fn diff_hunks(old: &[String], new: &[String]) -> Vec<Vec<Opcode>> {
    line_matcher(old, new).grouped_opcodes(CONTEXT)
}

/// The whole opcode as one block, plus a line-by-line pairing when the old and new sides have
/// the same number of lines (the engine keeps whichever classifies more cleanly).
pub fn candidate_units(op: Opcode) -> (Vec<Opcode>, Option<Vec<Opcode>>) {
    let n = op.i2 - op.i1;
    if op.tag == Tag::Replace && n == op.j2 - op.j1 && n > 1 {
        let pairs = (0..n)
            .map(|k| {
                Opcode::new(
                    Tag::Replace,
                    op.i1 + k,
                    op.i1 + k + 1,
                    op.j1 + k,
                    op.j1 + k + 1,
                )
            })
            .collect();
        return (vec![op], Some(pairs));
    }
    (vec![op], None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_units_pairs_equal_sized_replaces() {
        let op = Opcode::new(Tag::Replace, 2, 4, 5, 7);
        let (block, pairs) = candidate_units(op);
        assert_eq!(block, vec![op]);
        assert_eq!(
            pairs.unwrap(),
            vec![
                Opcode::new(Tag::Replace, 2, 3, 5, 6),
                Opcode::new(Tag::Replace, 3, 4, 6, 7)
            ]
        );
        assert!(
            candidate_units(Opcode::new(Tag::Replace, 2, 3, 5, 6))
                .1
                .is_none()
        );
        assert!(
            candidate_units(Opcode::new(Tag::Delete, 2, 4, 5, 5))
                .1
                .is_none()
        );
    }
}
