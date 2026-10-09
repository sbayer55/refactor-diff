//! Render a report as Markdown: the review summary to paste into a PR or a message.

use std::collections::HashSet;

use crate::model::{Hunk, HunkLine, LineType, Report, SignatureKind};

/// Which groups and hunks the reviewer has marked as done.
#[derive(Clone, Debug, Default)]
pub struct ReviewMarks {
    pub groups: HashSet<String>,
    pub hunks: HashSet<String>,
}

impl ReviewMarks {
    pub fn new<I, J>(groups: I, hunks: J) -> Self
    where
        I: IntoIterator,
        I::Item: Into<String>,
        J: IntoIterator,
        J::Item: Into<String>,
    {
        Self {
            groups: groups.into_iter().map(Into::into).collect(),
            hunks: hunks.into_iter().map(Into::into).collect(),
        }
    }
}

/// The line a comment on this hunk should attach to: the first changed line of a unit that
/// still needs review, preferring the new side.
pub fn anchor_line<'r>(report: &'r Report, hunk: &'r Hunk) -> &'r HunkLine {
    let changed: Vec<&HunkLine> = hunk.lines.iter().filter(|l| l.is_change()).collect();
    let residual: Vec<&HunkLine> = changed
        .iter()
        .copied()
        .filter(|l| {
            l.unit
                .as_ref()
                .is_some_and(|u| report.units.get(u).is_some_and(|u| !u.explained))
        })
        .collect();
    let pool: Vec<&HunkLine> = if !residual.is_empty() {
        residual
    } else if !changed.is_empty() {
        changed
    } else {
        hunk.lines.iter().collect()
    };
    pool.iter()
        .copied()
        .find(|l| l.kind == LineType::Added)
        .unwrap_or(pool[0])
}

pub fn markdown_summary(report: &Report, review: Option<&ReviewMarks>) -> String {
    let empty = ReviewMarks::default();
    let review = review.unwrap_or(&empty);
    let src = &report.source;
    let stats = report.stats();
    let sha = |s: Option<&str>| match s {
        Some(s) if !s.is_empty() => format!("`{}`", s.chars().take(7).collect::<String>()),
        _ => "working tree".to_string(),
    };

    let mut out: Vec<String> = vec![format!("# refactor-diff: {}", src.label), String::new()];
    if let Some(url) = src
        .pr
        .as_ref()
        .and_then(|pr| pr.get("url"))
        .and_then(|u| u.as_str())
    {
        if !url.is_empty() {
            out.push(format!("{url} · "));
        }
    }
    let changes = if stats.residual_units == 1 {
        "change"
    } else {
        "changes"
    };
    let patterns = if stats.mechanical_groups == 1 {
        "pattern"
    } else {
        "patterns"
    };
    let verified = if stats.verified_units > 0 {
        format!(" · {} verified by AST", stats.verified_units)
    } else {
        String::new()
    };
    out.push(format!(
        "{} → {} · **{}%** of changed lines collapsed · **{}** {changes} to review · {} mechanical {patterns} · {}/{} files analyzed{verified}",
        sha(Some(&src.base_sha)),
        sha(src.head_sha.as_deref()),
        stats.collapsed_pct,
        stats.residual_units,
        stats.mechanical_groups,
        stats.files_analyzed,
        stats.files_changed,
    ));

    let trivial_kind =
        |k: SignatureKind| matches!(k, SignatureKind::Formatting | SignatureKind::Docs);
    let mech = report
        .groups
        .iter()
        .filter(|g| g.mechanical && !trivial_kind(g.kind));
    let trivial = report
        .groups
        .iter()
        .filter(|g| g.mechanical && trivial_kind(g.kind));
    let rows: Vec<_> = mech.chain(trivial).collect();
    if !rows.is_empty() {
        out.push(String::new());
        out.push("## Mechanical patterns".into());
        out.push(String::new());
        out.push("| | Kind | Pattern | Count | Files |".into());
        out.push("|---|---|---|---|---|".into());
        for g in rows {
            let tick = if review.groups.contains(&g.id) {
                "✓"
            } else {
                ""
            };
            out.push(format!(
                "| {tick} | {} | `{}` | {} | {} |",
                g.kind.as_str(),
                cell(&g.label),
                g.unit_ids.len(),
                g.files.len()
            ));
        }
    }

    out.push(String::new());
    out.push("## Needs review".into());
    out.push(String::new());
    let residual: Vec<&Hunk> = report
        .residual_hunk_ids
        .iter()
        .filter_map(|h| report.hunks.get(h))
        .collect();
    if residual.is_empty() {
        out.push("Nothing: every change matched a mechanical pattern.".into());
    }
    for h in residual {
        let first = anchor_line(report, h);
        let line = first.new_no.or(first.old_no).unwrap_or(h.new_start);
        let near = near_note(report, h);
        let boxed = if review.hunks.contains(&h.fingerprint) {
            "[x]"
        } else {
            "[ ]"
        };
        out.push(format!(
            "- {boxed} `{}:{line}` — `{}`{near}",
            h.path,
            cell(first.text.trim())
        ));
    }

    if !report.warnings.is_empty() {
        out.push(String::new());
        out.push("## Warnings".into());
        out.push(String::new());
        for w in &report.warnings {
            let where_: Vec<String> = w
                .locations
                .iter()
                .take(5)
                .map(|l| format!("`{}:{}`", l.path, l.line))
                .collect();
            let more = if w.total > w.locations.len() {
                format!(" … +{}", w.total - w.locations.len())
            } else {
                String::new()
            };
            let suffix = if where_.is_empty() {
                String::new()
            } else {
                format!(" ({}{more})", where_.join(", "))
            };
            out.push(format!(
                "- **{}**: {}{suffix}",
                serde_kind(w.kind),
                w.message
            ));
        }
    }
    out.join("\n") + "\n"
}

fn serde_kind(kind: crate::model::WarningKind) -> &'static str {
    match kind {
        crate::model::WarningKind::MissedRename => "missed-rename",
        crate::model::WarningKind::InconsistentRename => "inconsistent-rename",
        crate::model::WarningKind::NearMiss => "near-miss",
    }
}

fn near_note(report: &Report, hunk: &Hunk) -> String {
    for uid in &hunk.unit_ids {
        let Some(u) = report.units.get(uid) else {
            continue;
        };
        for n in &u.near {
            if let Some(g) = report.group(&n.group_id) {
                return format!(" — ≈ almost `{}`", cell(&g.label));
            }
        }
    }
    String::new()
}

fn cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace('`', "'")
        .replace('\n', " ")
}
