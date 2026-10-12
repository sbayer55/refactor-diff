//! Fold ranges: the blocks of a file the viewer can collapse (functions, classes, statement
//! bodies, multi-line literals, runs of imports or comments).
//!
//! Python, TypeScript and JavaScript use their tree-sitter grammars; every other file, and a
//! file the grammar can't parse cleanly, folds by indentation. Like code issues, folds are
//! served on their own endpoint and never touch the report.

use std::collections::BTreeMap;

use serde::Serialize;
use tree_sitter::Node;

use crate::lang::{Lang, parse};
use crate::text::split_lines;

/// One foldable block in 1-based lines: `start` stays visible as the fold's header and
/// `start + 1..=end` are hidden when it's folded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Fold {
    pub start: usize,
    pub end: usize,
}

/// Python nodes that fold from their own first line to their last.
const PY_SPANS: &[&str] = &[
    "dictionary",
    "list",
    "tuple",
    "set",
    "argument_list",
    "parameters",
    "string",
    "dictionary_comprehension",
    "list_comprehension",
    "set_comprehension",
    "generator_expression",
];

const PY_IMPORTS: &[&str] = &[
    "import_statement",
    "import_from_statement",
    "future_import_statement",
];

/// TypeScript / JavaScript nodes that fold from their own first line to their last.
const TS_SPANS: &[&str] = &[
    "statement_block",
    "class_body",
    "object",
    "array",
    "object_type",
    "interface_body",
    "enum_body",
    "switch_body",
    "arguments",
    "formal_parameters",
    "template_string",
    "jsx_element",
    "object_pattern",
    "array_pattern",
    "named_imports",
    "comment",
];

const TS_IMPORTS: &[&str] = &["import_statement"];

/// Foldable blocks in one file, sorted by start line, at most one per start line (the
/// largest).
pub fn find_folds(path: &str, text: &str) -> Vec<Fold> {
    let lines = split_lines(text);
    let tree = Lang::for_path(path).and_then(|lang| Some((lang, parse(&lang.grammar(), text)?)));
    let raw = match &tree {
        Some((lang, tree)) if !tree.root_node().has_error() => {
            syntax_folds(*lang, tree.root_node(), &lines)
        }
        _ => indent_folds(&lines),
    };
    normalize(raw)
}

/// Keeps the largest fold per start line and drops the ones that hide nothing.
fn normalize(raw: Vec<Fold>) -> Vec<Fold> {
    let mut by_start: BTreeMap<usize, usize> = BTreeMap::new();
    for f in raw {
        if f.end > f.start {
            let end = by_start.entry(f.start).or_insert(f.end);
            *end = (*end).max(f.end);
        }
    }
    by_start
        .into_iter()
        .map(|(start, end)| Fold { start, end })
        .collect()
}

fn syntax_folds(lang: Lang, root: Node<'_>, lines: &[String]) -> Vec<Fold> {
    let (spans, imports) = if lang.is_python() {
        (PY_SPANS, PY_IMPORTS)
    } else {
        (TS_SPANS, TS_IMPORTS)
    };
    let mut out = vec![];
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if lang.is_python() && kind == "block" {
            // A body folds from its parent's header (`def …:`, `else:`, `except …:`), so each
            // clause of an if / try folds on its own.
            if let Some(parent) = node.parent() {
                out.push(Fold {
                    start: parent.start_position().row + 1,
                    end: node.end_position().row + 1,
                });
            }
        } else if spans.contains(&kind) {
            out.push(span(
                node.start_position().row,
                node.end_position().row,
                lines,
            ));
        }
        if node.child_count() > 0 {
            out.extend(runs(node, imports, lines));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    out
}

/// A fold over 0-based rows `start..=end`. When the last line opens with a closer (`}`, `]`,
/// `)`, `</`), that line stays visible, as in editors.
fn span(start: usize, end: usize, lines: &[String]) -> Fold {
    let last = lines.get(end).map(|l| l.trim_start()).unwrap_or("");
    let closes = ["}", "]", ")", "</"].iter().any(|c| last.starts_with(c));
    let end = if closes && end > start { end - 1 } else { end };
    Fold {
        start: start + 1,
        end: end + 1,
    }
}

/// Runs of two or more sibling imports (comments between them don't break the run), and of
/// two or more comments that each sit alone on consecutive lines, under `parent`.
fn runs(parent: Node<'_>, imports: &[&str], lines: &[String]) -> Vec<Fold> {
    let mut out = vec![];
    let mut import_run = Run::default();
    let mut comment_run = Run::default();
    let mut cursor = parent.walk();
    for child in parent.named_children(&mut cursor) {
        let (row, end_row) = (child.start_position().row, child.end_position().row);
        if imports.contains(&child.kind()) {
            import_run.add(row, end_row);
            comment_run.flush(&mut out);
        } else if child.kind() == "comment" && row == end_row && alone(child, lines) {
            if comment_run.n > 0 && row != comment_run.last + 1 {
                comment_run.flush(&mut out);
            }
            comment_run.add(row, end_row);
        } else {
            import_run.flush(&mut out);
            comment_run.flush(&mut out);
        }
    }
    import_run.flush(&mut out);
    comment_run.flush(&mut out);
    out
}

/// A run of sibling nodes over 0-based rows `first..=last`.
#[derive(Default)]
struct Run {
    first: usize,
    last: usize,
    n: usize,
}

impl Run {
    fn add(&mut self, row: usize, end_row: usize) {
        if self.n == 0 {
            self.first = row;
        }
        self.last = end_row;
        self.n += 1;
    }

    fn flush(&mut self, out: &mut Vec<Fold>) {
        if self.n >= 2 {
            out.push(Fold {
                start: self.first + 1,
                end: self.last + 1,
            });
        }
        *self = Run::default();
    }
}

/// Whether nothing but whitespace comes before `node` on its line.
fn alone(node: Node<'_>, lines: &[String]) -> bool {
    let pos = node.start_position();
    lines
        .get(pos.row)
        .and_then(|l| l.as_bytes().get(..pos.column))
        .is_some_and(|before| before.iter().all(u8::is_ascii_whitespace))
}

/// Indentation folds: a line folds the lines after it that are indented deeper, up to the
/// last such non-blank line.
fn indent_folds(lines: &[String]) -> Vec<Fold> {
    let depth: Vec<Option<usize>> = lines.iter().map(|l| indent_of(l)).collect();
    let mut out = vec![];
    for (i, d) in depth.iter().enumerate() {
        let Some(d) = *d else { continue };
        let mut end = i;
        for (j, dj) in depth.iter().enumerate().skip(i + 1) {
            match dj {
                None => continue,
                Some(dj) if *dj > d => end = j,
                Some(_) => break,
            }
        }
        if end > i {
            out.push(Fold {
                start: i + 1,
                end: end + 1,
            });
        }
    }
    out
}

/// A line's indentation width (tabs to the next multiple of 4), or `None` for a blank line.
fn indent_of(line: &str) -> Option<usize> {
    let mut width = 0;
    for ch in line.chars() {
        match ch {
            ' ' => width += 1,
            '\t' => width += 4 - width % 4,
            '\r' | '\n' => return None,
            _ => return Some(width),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folds(path: &str, text: &str) -> Vec<(usize, usize)> {
        find_folds(path, text)
            .into_iter()
            .map(|f| (f.start, f.end))
            .collect()
    }

    #[test]
    fn python_bodies_fold_from_their_header() {
        let src = "\
class A:
    def f(self, x):
        if x:
            return 1
        elif x is None:
            return 2
        else:
            y = 3
            return y
";
        assert_eq!(
            folds("a.py", src),
            vec![(1, 9), (2, 9), (3, 4), (5, 6), (7, 9)]
        );
    }

    #[test]
    fn python_literals_and_docstrings() {
        let src = "\
def f():
    \"\"\"Doc
    string.\"\"\"
    return {
        'a': 1,
    }
";
        assert_eq!(folds("a.py", src), vec![(1, 6), (2, 3), (4, 5)]);
    }

    #[test]
    fn python_import_and_comment_runs() {
        let src = "\
import os
# why
import sys
from x import y

# one
# two
x = 1  # trailing
";
        assert_eq!(folds("a.py", src), vec![(1, 4), (6, 7)]);
    }

    #[test]
    fn typescript_keeps_the_closer_visible() {
        let src = "\
import { a } from 'a';
import { b } from 'b';
export class C {
  m(x: number) {
    if (x) {
      return 1;
    } else {
      return 2;
    }
  }
}
const o = {
  k: 1,
};
";
        assert_eq!(
            folds("a.ts", src),
            vec![(1, 2), (3, 10), (4, 9), (5, 6), (7, 8), (12, 13)]
        );
    }

    #[test]
    fn javascript_block_comment() {
        let src = "/**\n * Doc.\n */\nfunction f() {\n  return 1;\n}\n";
        assert_eq!(folds("a.js", src), vec![(1, 3), (4, 5)]);
    }

    #[test]
    fn other_languages_fold_by_indentation() {
        let src = "fn main() {\n    let x = 1;\n\n    if x {\n    \ty();\n    }\n}\n";
        assert_eq!(folds("a.rs", src), vec![(1, 6), (4, 5)]);
    }

    #[test]
    fn broken_files_fold_by_indentation() {
        let src = "def f(:\n    return 1\n";
        assert_eq!(folds("a.py", src), vec![(1, 2)]);
    }

    #[test]
    fn empty_and_flat_files_have_no_folds() {
        assert!(folds("a.py", "").is_empty());
        assert!(folds("a.txt", "a\nb\n").is_empty());
        assert!(folds("a.py", "x = 1\ny = 2\n").is_empty());
    }
}
