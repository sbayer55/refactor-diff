//! The state object one hunk is evaluated against: the hunk, its surroundings and the facts
//! the tool already knows (file category, whether tests were added, ...).

use refactor_diff_core::{Category, LineType, SignatureKind};
use serde_json::{Value, json};

use crate::ai::context::{ContextBuilder, ContextError, Span, fence_lang};

/// Lines per side of the enclosing function sent to Jev (the Ask feature sends up to
/// [`crate::ai::context::FUNCTION_LINE_CAP`]).
pub const FUNCTION_LINE_CAP: usize = 120;
pub const TEST_FILES_CAP: usize = 20;
pub const PATTERNS_CAP: usize = 5;
/// Characters of the PR description sent as `pr_body`.
pub const PR_BODY_CAP: usize = 2000;

/// Facts computed by the tool rather than asked of Jev; kept on the risk record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Evidence {
    pub adds_tests: bool,
    pub docs_only: bool,
}

/// Build the state for one hunk. Parses the file for the enclosing function, so call it off
/// the async threads.
pub fn hunk_state(
    builder: &ContextBuilder,
    hunk_id: &str,
) -> Result<(Value, Evidence), ContextError> {
    let report = builder.report();
    let focus = builder.focus(hunk_id, None, None)?;
    let hunk = report
        .hunks
        .get(hunk_id)
        .ok_or_else(|| ContextError::new("Unknown hunk; run the analysis again."))?;
    let file = report.file(&hunk.path);
    let category = file.map_or(Category::Other, |f| f.category);
    let units: Vec<_> = hunk
        .unit_ids
        .iter()
        .filter_map(|id| report.units.get(id))
        .collect();
    let docs_only = category == Category::Docs
        || (!units.is_empty()
            && units.iter().all(|u| {
                !u.signatures.is_empty()
                    && u.signatures.iter().all(|s| s.kind == SignatureKind::Docs)
            }));
    let adds_tests =
        category == Category::Tests && hunk.lines.iter().any(|ln| ln.kind == LineType::Added);
    let evidence = Evidence {
        adds_tests,
        docs_only,
    };

    let changed_test_files: Vec<&str> = report
        .files
        .iter()
        .filter(|f| f.category == Category::Tests)
        .map(|f| f.path.as_str())
        .take(TEST_FILES_CAP)
        .collect();
    let patterns: Vec<String> = builder
        .patterns_piece()
        .into_iter()
        .take(PATTERNS_CAP)
        .collect();
    let enclosing = builder.function_piece(&focus).map(|f| {
        json!({
            "kind": f.kind_str(),
            "qualname": f.qualname,
            "before": f.old.as_ref().map(capped),
            "after": f.new.as_ref().map(capped),
        })
    });
    let pr = builder.pr_piece();
    let mut state = json!({
        "path": hunk.path,
        "category": category.as_str(),
        "language": fence_lang(&hunk.path),
        "docs_only": docs_only,
        "adds_tests": adds_tests,
        "changed_test_files": changed_test_files,
        "pr_title": pr.as_ref().map(|p| p.title.clone()),
        "patterns": patterns,
        "enclosing": enclosing,
        "hunk": builder.hunk_piece(&focus),
    });
    if let Some(body) = pr
        .as_ref()
        .map(|p| p.body.as_str())
        .filter(|b| !b.is_empty())
    {
        state["pr_body"] = Value::String(cut_chars(body, PR_BODY_CAP));
    }
    if let Some(stated) = stated_change(builder) {
        state["stated_change"] = Value::String(stated);
    }
    Ok((state, evidence))
}

/// What the change says it does: the PR title, else "removes `stem`" for the strongest
/// concept, else nothing.
pub fn stated_change(builder: &ContextBuilder) -> Option<String> {
    if let Some(title) = builder
        .pr_piece()
        .map(|p| p.title.trim().to_string())
        .filter(|t| !t.is_empty())
    {
        return Some(title);
    }
    builder
        .report()
        .concepts
        .first()
        .filter(|c| !c.stem.is_empty())
        .map(|c| format!("removes `{}`", c.stem))
}

/// At most `cap` characters, the last one an ellipsis when cut.
fn cut_chars(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let mut out: String = text.chars().take(cap.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// A span's text, cut to [`FUNCTION_LINE_CAP`] lines.
fn capped(span: &Span) -> String {
    let mut lines: Vec<&str> = span.text.lines().collect();
    let cut = lines.len() > FUNCTION_LINE_CAP || span.truncated;
    lines.truncate(FUNCTION_LINE_CAP);
    let mut text = lines.join("\n");
    if cut {
        text.push_str("\n… (truncated)");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_chars_counts_characters() {
        assert_eq!(cut_chars("abc", 3), "abc");
        assert_eq!(cut_chars("abcd", 3), "ab…");
        let long = "é".repeat(PR_BODY_CAP + 10);
        assert_eq!(cut_chars(&long, PR_BODY_CAP).chars().count(), PR_BODY_CAP);
    }

    #[test]
    fn capped_marks_long_spans() {
        let span = Span {
            path: "a.py".into(),
            start: 1,
            end: 200,
            text: (0..200)
                .map(|i| format!("line {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            truncated: false,
        };
        let text = capped(&span);
        assert_eq!(text.lines().count(), FUNCTION_LINE_CAP + 1);
        assert!(text.ends_with("… (truncated)"));
        let short = Span {
            text: "a\nb".into(),
            ..span
        };
        assert_eq!(capped(&short), "a\nb");
    }
}
