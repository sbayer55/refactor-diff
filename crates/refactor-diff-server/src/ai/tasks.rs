//! The predefined Ask tasks: what each one sends and asks for.
//!
//! A task names the context pieces it needs, says when it is unavailable for a location (so
//! the menu can grey it out with a reason), gives the small hint shown next to its menu row,
//! and adds a few sentences of instruction to the shared system prompt.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::context::{ContextBuilder, Focus, Function};

/// A piece of context. Variants are in alphabetical order so `Ord` sorts like Python's
/// `sorted()` on the names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Piece {
    Function,
    History,
    Hunk,
    Patterns,
    Pr,
    References,
}

impl Piece {
    pub const ALL: [Piece; 6] = [
        Piece::Hunk,
        Piece::Function,
        Piece::References,
        Piece::History,
        Piece::Pr,
        Piece::Patterns,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Piece::Function => "function",
            Piece::History => "history",
            Piece::Hunk => "hunk",
            Piece::Patterns => "patterns",
            Piece::Pr => "pr",
            Piece::References => "references",
        }
    }
}

impl fmt::Display for Piece {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Piece {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Piece::ALL
            .iter()
            .copied()
            .find(|p| p.as_str() == s)
            .ok_or(())
    }
}

pub const SYSTEM_PROMPT: &str =
    "You are helping a developer review a code change in refactor-diff, a tool that
collapses repeated mechanical edits (renames, type changes, identical substitutions) so the
reviewer only reads what is left.

The user message contains context blocks, each under a `## heading`:
- The hunk is a unified diff with old and new line numbers in the first two columns and `-`/`+`
  in the third. Lines in [[double brackets]] are annotations from refactor-diff, not code:
  `pattern:` means a repeated mechanical edit already explains that unit, `verified:` means the
  enclosing statement was proven equivalent on both sides, `near miss:` means the change almost
  matches a pattern (often a typo), `unique` means a one-off edit.
- The enclosing function or class may be given before and after the change.
- References list where a name is used, as `path:line  code`.
- Commits and the pull request describe why the change was made, when available.

Rules for your answer:
- Lead with the answer. Short paragraphs or a numbered list; no headings; no restating code.
- Focus on the lines that are not explained by a pattern unless the question is about the
  pattern itself; say when a pattern already covers something.
- Cite lines as `L<n>` for new-side line numbers and `O<n>` for old-side line numbers, and
  ranges as `L17–18`. Name other files as `path:line`.
- Be concrete about behaviour: inputs, outputs, errors, side effects. Say \"I can't tell from
  the context\" rather than guessing when the context is insufficient.
- Keep it under 250 words unless the question needs more.
";

pub const JUDGEMENT_SUFFIX: &str = " End with one line that starts with `Verdict:` saying whether anything needs the \
reviewer's attention.";

type Rule<T> = fn(&ContextBuilder, &Focus, Option<&Function>) -> T;

#[derive(Clone)]
pub struct Task {
    pub id: &'static str,
    pub label: &'static str,
    /// "Understand" | "Review and risk" | "Custom"
    pub group: &'static str,
    pub pieces: &'static [Piece],
    pub instruction: &'static str,
    unavailable: Rule<Option<String>>,
    hint: Rule<String>,
    /// End with a one-line verdict.
    pub judgement: bool,
    pub needs_function: bool,
    pub needs_both_sides: bool,
}

impl fmt::Debug for Task {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Task").field("id", &self.id).finish()
    }
}

impl Task {
    /// The pieces this task sends.
    pub fn pieces(&self) -> BTreeSet<Piece> {
        self.pieces.iter().copied().collect()
    }

    /// Why the task can't run at this location, or `None` when it can.
    pub fn unavailable(
        &self,
        builder: &ContextBuilder,
        focus: &Focus,
        func: Option<&Function>,
    ) -> Option<String> {
        (self.unavailable)(builder, focus, func)
    }

    /// The small hint shown next to the menu row.
    pub fn hint(&self, builder: &ContextBuilder, focus: &Focus, func: Option<&Function>) -> String {
        (self.hint)(builder, focus, func)
    }

    pub fn is_custom(&self) -> bool {
        self.id == CUSTOM.id
    }
}

fn available(_: &ContextBuilder, _: &Focus, _: Option<&Function>) -> Option<String> {
    None
}

fn no_hint(_: &ContextBuilder, _: &Focus, _: Option<&Function>) -> String {
    String::new()
}

fn no_def(_: &ContextBuilder, _: &Focus, func: Option<&Function>) -> Option<String> {
    if func.is_none() {
        return Some("the line isn't inside a function or class".to_string());
    }
    None
}

fn no_both_sides(_: &ContextBuilder, _: &Focus, func: Option<&Function>) -> Option<String> {
    let Some(func) = func else {
        return Some("the line isn't inside a function or class".to_string());
    };
    if func.old.is_none() {
        return Some(format!("{} is new in this diff", func.name()));
    }
    if func.new.is_none() {
        return Some(format!("{} was removed in this diff", func.name()));
    }
    None
}

fn no_refs(b: &ContextBuilder, f: &Focus, func: Option<&Function>) -> Option<String> {
    if let Some(why) = b.references_available(f) {
        return Some(why);
    }
    no_def(b, f, func)
}

fn no_history(b: &ContextBuilder, _: &Focus, _: Option<&Function>) -> Option<String> {
    let src = &b.report().source;
    let has_pr = src.pr.as_ref().is_some_and(crate::settings::truthy);
    if src.head_sha.as_deref().is_none_or(str::is_empty) && !has_pr {
        return Some("no commits or pull request for the working tree".to_string());
    }
    None
}

fn hint_unexplained(b: &ContextBuilder, f: &Focus, _: Option<&Function>) -> String {
    let n = b.unexplained_lines(f);
    format!("{n} unexplained line{}", if n != 1 { "s" } else { "" })
}

fn hint_fn(_: &ContextBuilder, _: &Focus, func: Option<&Function>) -> String {
    match func {
        Some(f) if f.kind == refactor_diff_core::StmtKind::Def => format!("{}()", f.name()),
        Some(f) => f.qualname.clone(),
        None => String::new(),
    }
}

fn hint_history(b: &ContextBuilder, f: &Focus, _: Option<&Function>) -> String {
    let src = &b.report().source;
    let mut parts: Vec<String> = Vec::new();
    if let Some(pr) = src.pr.as_ref().filter(|pr| crate::settings::truthy(pr)) {
        let number = match &pr["number"] {
            serde_json::Value::Null => "None".to_string(),
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        parts.push(format!("PR #{number}"));
    }
    let commits = if src.head_sha.as_deref().is_some_and(|s| !s.is_empty()) {
        b.history_piece(f).range.len()
    } else {
        0
    };
    if commits > 0 {
        parts.push(format!(
            "{commits} commit{}",
            if commits != 1 { "s" } else { "" }
        ));
    }
    parts.join(" · ")
}

fn hint_verified(b: &ContextBuilder, f: &Focus, _: Option<&Function>) -> String {
    let report = b.report();
    let Some(hunk) = report.hunks.get(&f.hunk_id) else {
        return "not verified".to_string();
    };
    let units: Vec<_> = hunk
        .lines
        .iter()
        .filter_map(|ln| ln.unit.as_deref())
        .filter_map(|u| report.units.get(u))
        .collect();
    if !units.is_empty() && units.iter().all(|u| u.verified) {
        "verified".to_string()
    } else {
        "not verified".to_string()
    }
}

pub static TASKS: [Task; 8] = [
    Task {
        id: "explain",
        label: "Explain this change",
        group: "Understand",
        pieces: &[Piece::Hunk, Piece::Function, Piece::Pr, Piece::Patterns],
        instruction: "Explain what this hunk changes and the intended effect. Separate the mechanical part \
(covered by patterns) from the substantive part in one sentence, then explain the \
substantive part: what behaves differently and for whom. End with anything a reviewer \
should double-check, if there is something.",
        unavailable: available,
        hint: hint_unexplained,
        judgement: false,
        needs_function: false,
        needs_both_sides: false,
    },
    Task {
        id: "function",
        label: "How does this function work",
        group: "Understand",
        pieces: &[Piece::Hunk, Piece::Function],
        instruction: "Explain how the enclosing function works in its current (after) form: its inputs, what it \
returns, the main steps, error paths and side effects. Mention the change only where it \
matters to the explanation.",
        unavailable: no_def,
        hint: hint_fn,
        judgement: false,
        needs_function: true,
        needs_both_sides: false,
    },
    Task {
        id: "compare",
        label: "Compare old vs new",
        group: "Understand",
        pieces: &[Piece::Hunk, Piece::Function],
        instruction: "Compare the before and after versions of the enclosing function semantically: what it \
used to do, what it does now, and every observable difference (return values, errors, \
side effects, performance). Ignore differences that are only renames or formatting.",
        unavailable: no_both_sides,
        hint: hint_fn,
        judgement: false,
        needs_function: false,
        needs_both_sides: true,
    },
    Task {
        id: "uses",
        label: "Who uses this",
        group: "Understand",
        pieces: &[Piece::Hunk, Piece::Function, Piece::References],
        instruction: "Summarise how the enclosing function is used, from the references: group callers by \
purpose, say what each group relies on (return shape, null handling, exceptions), and \
point out any caller that this change affects.",
        unavailable: no_refs,
        hint: hint_fn,
        judgement: false,
        needs_function: true,
        needs_both_sides: false,
    },
    Task {
        id: "why",
        label: "Why was this changed",
        group: "Understand",
        pieces: &[Piece::Hunk, Piece::History, Piece::Pr],
        instruction: "Explain why this line was changed, using the commit messages and the pull request \
description. Say which commit introduced it. If the stated reason doesn't match what the \
code does, say so.",
        unavailable: no_history,
        hint: hint_history,
        judgement: false,
        needs_function: false,
        needs_both_sides: false,
    },
    Task {
        id: "review",
        label: "Review this change",
        group: "Review and risk",
        pieces: &[Piece::Hunk, Piece::Function, Piece::Pr, Piece::Patterns],
        instruction: "Review the substantive part of this hunk as a careful colleague: bugs, unhandled cases, \
behaviour changes that callers may not expect, and anything inconsistent with the \
rest of the diff. Number the findings, most important first; skip style nits.",
        unavailable: available,
        hint: hint_unexplained,
        judgement: true,
        needs_function: false,
        needs_both_sides: false,
    },
    Task {
        id: "preserving",
        label: "Is this behavior-preserving",
        group: "Review and risk",
        pieces: &[Piece::Hunk, Piece::Function],
        instruction: "Decide whether the change to the enclosing function preserves behaviour for every input. \
Reason from the before and after versions; list each difference you can find with the \
input that exposes it. Be strict: a renamed function with one extra guard is not \
behaviour-preserving.",
        unavailable: no_both_sides,
        hint: hint_verified,
        judgement: true,
        needs_function: false,
        needs_both_sides: true,
    },
    Task {
        id: "break",
        label: "What could break",
        group: "Review and risk",
        pieces: &[Piece::Hunk, Piece::Function, Piece::References],
        instruction: "Assess the blast radius of this change: which callers and code paths are affected, \
which of them could now fail or behave differently, and what should be tested. Use the \
references list; name the files and lines.",
        unavailable: no_refs,
        hint: hint_fn,
        judgement: true,
        needs_function: true,
        needs_both_sides: false,
    },
];

pub static CUSTOM: Task = Task {
    id: "custom",
    label: "Custom prompt",
    group: "Custom",
    pieces: &[Piece::Hunk, Piece::Patterns],
    instruction: "Answer the developer's question about this location using the context provided.",
    unavailable: available,
    hint: no_hint,
    judgement: false,
    needs_function: false,
    needs_both_sides: false,
};

/// A menu task or the custom prompt, by id.
pub fn by_id(id: &str) -> Option<&'static Task> {
    TASKS
        .iter()
        .chain(std::iter::once(&CUSTOM))
        .find(|t| t.id == id)
}

pub fn system_prompt(task: &Task) -> String {
    let suffix = if task.judgement { JUDGEMENT_SUFFIX } else { "" };
    format!("{SYSTEM_PROMPT}\nTask: {}{suffix}", task.instruction)
}

/// One row of the Ask menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MenuRow {
    pub id: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub hint: String,
    pub unavailable: Option<String>,
}

/// The menu rows for a location: label, group, hint and why a task is unavailable.
pub fn menu(builder: &ContextBuilder, focus: &Focus) -> Vec<MenuRow> {
    let func = builder.function_piece(focus);
    menu_with(builder, focus, func.as_ref())
}

/// [`menu`] for a function piece the caller already computed.
pub fn menu_with(builder: &ContextBuilder, focus: &Focus, func: Option<&Function>) -> Vec<MenuRow> {
    TASKS
        .iter()
        .map(|t| MenuRow {
            id: t.id,
            label: t.label,
            group: t.group,
            hint: t.hint(builder, focus, func),
            unavailable: t.unavailable(builder, focus, func),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pieces_sort_alphabetically_and_serialize_lowercase() {
        let mut v = vec![Piece::References, Piece::Hunk, Piece::Function, Piece::Pr];
        v.sort();
        assert_eq!(
            v,
            [Piece::Function, Piece::Hunk, Piece::Pr, Piece::References]
        );
        assert_eq!(
            serde_json::to_string(&v).unwrap(),
            r#"["function","hunk","pr","references"]"#
        );
        assert_eq!("patterns".parse::<Piece>(), Ok(Piece::Patterns));
        assert!("nope".parse::<Piece>().is_err());
    }

    #[test]
    fn system_prompt_adds_the_task_and_verdict() {
        let explain = by_id("explain").unwrap();
        let text = system_prompt(explain);
        assert!(text.starts_with(SYSTEM_PROMPT));
        assert!(text.contains("Task: Explain what this hunk changes"));
        assert!(!text.contains("Verdict:"));
        assert!(system_prompt(by_id("review").unwrap()).contains("Verdict:"));
        assert!(by_id("custom").unwrap().is_custom());
        assert!(by_id("nope").is_none());
        assert_eq!(
            TASKS.iter().map(|t| t.id).collect::<Vec<_>>(),
            [
                "explain",
                "function",
                "compare",
                "uses",
                "why",
                "review",
                "preserving",
                "break"
            ]
        );
    }

    #[test]
    fn prompt_text_matches_python_verbatim() {
        assert!(
            SYSTEM_PROMPT.ends_with("- Keep it under 250 words unless the question needs more.\n")
        );
        assert!(
            SYSTEM_PROMPT.contains("Say \"I can't tell from\n  the context\" rather than guessing")
        );
        assert_eq!(
            JUDGEMENT_SUFFIX,
            " End with one line that starts with `Verdict:` saying whether anything needs the reviewer's attention."
        );
        assert_eq!(
            CUSTOM.pieces(),
            [Piece::Hunk, Piece::Patterns].into_iter().collect()
        );
    }
}
