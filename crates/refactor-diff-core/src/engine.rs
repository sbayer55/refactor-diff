//! Turn loaded file changes into a [`Report`]. UI-agnostic and git-free: the server loads the
//! changes and answers the engine's few questions about the head revision.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use indexmap::IndexMap;

use crate::categories::categorize;
use crate::classify::{Classification, classify, imports_only, make_unit};
use crate::grouping::build_groups;
use crate::hunks::{candidate_units, diff_hunks};
use crate::lang::{FileAnalysis, LanguageAnalyzer, analyzer_for};
use crate::model::{
    FileChange, FileStatus, FileSummary, Group, Hunk, HunkLine, LineType, Report, SignatureKind,
    Source, Unit, UnitTag, Warning,
};
use crate::moves::{
    Analyses, detect_moves, link_file_renames, link_imports, resync_hunks, tag_moves,
};
use crate::seqmatch::{Opcode, Tag};
use crate::short_hash;
use crate::text::split_lines;
use crate::verify::verify_units;
use crate::warnings::{find_leftovers, inconsistent_renames, near_misses, still_defined};

/// What the engine needs to know about the head revision beyond the changed files: used to
/// find references left behind by a renamed definition.
pub trait HeadFiles {
    /// Paths whose head-side content contains `word` as a whole word, limited to files
    /// matching `globs` (git pathspecs such as `*.py`). Any order; the engine sorts.
    fn grep_word(&self, word: &str, globs: &[&str]) -> Vec<String>;
    /// Head-side contents of `paths`; missing files are omitted.
    fn read(&self, paths: &[&str]) -> HashMap<String, String>;
}

/// A head that answers nothing: no missed-rename warnings.
pub struct NoHeadFiles;

impl HeadFiles for NoHeadFiles {
    fn grep_word(&self, _word: &str, _globs: &[&str]) -> Vec<String> {
        vec![]
    }
    fn read(&self, _paths: &[&str]) -> HashMap<String, String> {
        HashMap::new()
    }
}

/// Analyze a change set. `source` describes what was compared; `min_count` is how many times
/// an edit must repeat to count as mechanical.
pub fn analyze(
    mut source: Source,
    mut changes: Vec<FileChange>,
    min_count: usize,
    head: &dyn HeadFiles,
) -> Report {
    source.min_count = min_count;
    changes.sort_by(|a, b| a.path.cmp(&b.path));

    let mut files: Vec<FileSummary> = Vec::new();
    let mut units: IndexMap<String, Unit> = IndexMap::new();
    let mut hunks: IndexMap<String, Hunk> = IndexMap::new();
    let mut analyses: Analyses = BTreeMap::new();
    let mut texts: BTreeMap<String, (String, String)> = BTreeMap::new();

    for change in &changes {
        let analyzer = analyzer_for(&change.path);
        let mut summary = FileSummary {
            path: change.path.clone(),
            old_path: change.old_path.clone(),
            status: change.status,
            analyzed: analyzer.is_some(),
            additions: 0,
            deletions: 0,
            units: 0,
            residual_units: 0,
            parse_ok: true,
            category: categorize(&change.path, analyzer.is_some()),
            pure_rename: change.status == FileStatus::Renamed && change.old_text == change.new_text,
        };
        texts.insert(
            change.path.clone(),
            (change.old_text.clone(), change.new_text.clone()),
        );
        let Some(analyzer) = analyzer else {
            let (old_lines, new_lines) =
                (split_lines(&change.old_text), split_lines(&change.new_text));
            for group in diff_hunks(&old_lines, &new_lines) {
                for op in group.iter().filter(|op| op.tag != Tag::Equal) {
                    summary.deletions += (op.i2 - op.i1) as u32;
                    summary.additions += (op.j2 - op.j1) as u32;
                }
            }
            files.push(summary);
            continue;
        };
        let old_an = analyzer.analyze(&change.old_text);
        let new_an = analyzer.analyze(&change.new_text);
        summary.parse_ok = old_an.parsed && new_an.parsed;
        let (file_units, file_hunks) = diff_file(change, analyzer, &old_an, &new_an, &mut summary);
        analyses.insert(change.path.clone(), (old_an, new_an));
        units.extend(file_units.into_iter().map(|u| (u.id.clone(), u)));
        hunks.extend(file_hunks.into_iter().map(|h| (h.id.clone(), h)));
        files.push(summary);
    }

    let mut moves = detect_moves(&mut units, &mut hunks, &analyses);
    link_imports(&mut units, &mut moves, &analyses);
    resync_hunks(&mut hunks, &units);
    tag_imports(&mut units, &analyses);
    tag_moves(&mut units, &moves);
    link_file_renames(&mut units, &files);
    let groups = build_groups(&mut units, min_count);
    verify_units(&mut units, &analyses, &groups);
    for f in &mut files {
        let mine = units.values().filter(|u| u.path == f.path);
        f.units = mine.clone().count();
        f.residual_units = mine.filter(|u| !u.explained).count();
    }

    let mut warnings = inconsistent_renames(&groups);
    warnings.extend(near_misses(&mut units, &groups));
    let head_analyses: BTreeMap<&str, &FileAnalysis> = analyses
        .iter()
        .map(|(p, (_, new_an))| (p.as_str(), new_an))
        .collect();
    warnings.extend(leftover_warnings(head, &groups, &head_analyses));

    let residual: Vec<String> = hunks
        .values()
        .filter(|h| h.unit_ids.iter().any(|uid| !units[uid].explained))
        .map(|h| h.id.clone())
        .collect();
    let id = short_hash!(
        source.base_sha,
        source.head_sha.as_deref().unwrap_or("worktree"),
        min_count
    );
    Report {
        id,
        source,
        files,
        groups,
        units,
        hunks,
        residual_hunk_ids: residual,
        warnings,
        texts,
    }
}

fn tag_imports(units: &mut IndexMap<String, Unit>, analyses: &Analyses) {
    for u in units.values_mut() {
        let (old_an, new_an) = &analyses[&u.path];
        let old_rng =
            (!u.old.is_empty()).then(|| (u.old_start, u.old_start + u.old.len() as u32 - 1));
        let new_rng =
            (!u.new.is_empty()).then(|| (u.new_start, u.new_start + u.new.len() as u32 - 1));
        if let Some(analyzer) = analyzer_for(&u.path) {
            if imports_only(analyzer, old_an, new_an, old_rng, new_rng) {
                u.tags.push(UnitTag::Imports);
            }
        }
    }
}

#[allow(clippy::needless_range_loop)] // indices are line numbers
fn diff_file(
    change: &FileChange,
    analyzer: &dyn LanguageAnalyzer,
    old_an: &FileAnalysis,
    new_an: &FileAnalysis,
    summary: &mut FileSummary,
) -> (Vec<Unit>, Vec<Hunk>) {
    let mut units: Vec<Unit> = Vec::new();
    let mut hunks: Vec<Hunk> = Vec::new();
    let (old_lines, new_lines) = (&old_an.lines, &new_an.lines);

    for group in diff_hunks(old_lines, new_lines) {
        let first = group[0];
        let hunk_id = short_hash!(change.path, first.i1, first.j1);
        let mut lines: Vec<HunkLine> = Vec::new();
        let mut hunk_units: Vec<String> = Vec::new();
        for op in group {
            if op.tag == Tag::Equal {
                for i in op.i1..op.i2 {
                    lines.push(HunkLine {
                        kind: LineType::Context,
                        text: old_lines[i].clone(),
                        old_no: Some(i as u32 + 1),
                        new_no: Some((op.j1 + (i - op.i1)) as u32 + 1),
                        unit: None,
                        hl: vec![],
                    });
                }
                continue;
            }
            summary.deletions += (op.i2 - op.i1) as u32;
            summary.additions += (op.j2 - op.j1) as u32;
            let op_units: Vec<Unit> = best_split(analyzer, old_an, new_an, op)
                .into_iter()
                .map(|(o, cls)| make_unit(&change.path, &hunk_id, &o, cls, old_lines, new_lines))
                .collect();
            // Unified-diff order: all removed lines of the opcode, then all added lines.
            let mut unit_of_old: HashMap<u32, &Unit> = HashMap::new();
            let mut unit_of_new: HashMap<u32, &Unit> = HashMap::new();
            for u in &op_units {
                for k in 0..u.old.len() as u32 {
                    unit_of_old.insert(u.old_start + k, u);
                }
                for k in 0..u.new.len() as u32 {
                    unit_of_new.insert(u.new_start + k, u);
                }
            }
            for i in op.i1..op.i2 {
                let n = i as u32 + 1;
                let u = unit_of_old[&n];
                let ln = &u.old[(n - u.old_start) as usize];
                lines.push(HunkLine {
                    kind: LineType::Removed,
                    text: ln.text.clone(),
                    old_no: Some(n),
                    new_no: None,
                    unit: Some(u.id.clone()),
                    hl: ln.hl.clone(),
                });
            }
            for j in op.j1..op.j2 {
                let n = j as u32 + 1;
                let u = unit_of_new[&n];
                let ln = &u.new[(n - u.new_start) as usize];
                lines.push(HunkLine {
                    kind: LineType::Added,
                    text: ln.text.clone(),
                    old_no: None,
                    new_no: Some(n),
                    unit: Some(u.id.clone()),
                    hl: ln.hl.clone(),
                });
            }
            hunk_units.extend(op_units.iter().map(|u| u.id.clone()));
            units.extend(op_units);
        }
        let changed: Vec<String> = lines
            .iter()
            .filter(|ln| ln.is_change())
            .map(|ln| {
                format!(
                    "{}{}",
                    if ln.kind == LineType::Removed {
                        "-"
                    } else {
                        "+"
                    },
                    ln.text
                )
            })
            .collect();
        let fingerprint = short_hash(std::iter::once(change.path.clone()).chain(changed));
        hunks.push(Hunk {
            id: hunk_id,
            path: change.path.clone(),
            old_start: first.i1 as u32 + 1,
            new_start: first.j1 as u32 + 1,
            lines,
            unit_ids: hunk_units,
            fingerprint,
        });
    }
    (units, hunks)
}

fn best_split(
    analyzer: &dyn LanguageAnalyzer,
    old_an: &FileAnalysis,
    new_an: &FileAnalysis,
    op: Opcode,
) -> Vec<(Opcode, Classification)> {
    let run = |ops: Vec<Opcode>| -> Vec<(Opcode, Classification)> {
        ops.into_iter()
            .map(|o| {
                let cls = classify(
                    analyzer,
                    old_an,
                    new_an,
                    o.old_range(),
                    o.new_range(),
                    o.i1 as u32,
                    o.j1 as u32,
                );
                (o, cls)
            })
            .collect()
    };
    let (block_ops, paired_ops) = candidate_units(op);
    let block = run(block_ops);
    let Some(paired_ops) = paired_ops else {
        return block;
    };
    let paired = run(paired_ops);
    // Line pairing is more granular; prefer it unless the lines don't really correspond
    // (e.g. a call re-wrapped across lines), which shows up as extra generic churn.
    let generic =
        |v: &[(Opcode, Classification)]| v.iter().map(|(_, c)| c.generic_tokens).sum::<usize>();
    if generic(&paired) > generic(&block) {
        block
    } else {
        paired
    }
}

/// Flag references left behind when a definition (def/class) was renamed.
///
/// Only renamed definitions are checked, repo-wide at head, and only when the old name is no
/// longer defined anywhere: then every remaining reference points at nothing. Renamed locals,
/// parameters and keyword arguments are skipped, since other variables with the same name are
/// usually unrelated.
fn leftover_warnings(
    head: &dyn HeadFiles,
    groups: &[Group],
    head_analyses: &BTreeMap<&str, &FileAnalysis>,
) -> Vec<Warning> {
    let mut warnings = Vec::new();
    let mut extra: HashMap<String, FileAnalysis> = HashMap::new();
    for g in groups {
        if g.kind != SignatureKind::Rename || !g.mechanical || !g.details.contains_key("definition")
        {
            continue;
        }
        let analyzers: Vec<Option<&dyn LanguageAnalyzer>> =
            g.files.iter().map(|p| analyzer_for(p)).collect();
        if analyzers.iter().flatten().any(|a| a.is_builtin(&g.old)) {
            continue;
        }
        let globs: BTreeSet<&str> = analyzers
            .iter()
            .flatten()
            .flat_map(|a| a.globs().iter().copied())
            .collect();
        let globs: Vec<&str> = globs.into_iter().collect();
        let hits = head.grep_word(&g.old, &globs);
        let missing: Vec<&str> = hits
            .iter()
            .map(String::as_str)
            .filter(|p| !head_analyses.contains_key(p) && !extra.contains_key(*p))
            .collect();
        for (path, text) in head.read(&missing) {
            if let Some(analyzer) = analyzer_for(&path) {
                extra.insert(path, analyzer.analyze(&text));
            }
        }
        let scope: BTreeMap<String, &FileAnalysis> = hits
            .iter()
            .filter_map(|p| {
                head_analyses
                    .get(p.as_str())
                    .copied()
                    .or_else(|| extra.get(p))
                    .map(|an| (p.clone(), an))
            })
            .collect();
        if scope
            .iter()
            .any(|(p, an)| analyzer_for(p).is_some_and(|a| still_defined(&g.old, an, a)))
        {
            continue;
        }
        if let Some(w) = find_leftovers(g, &scope) {
            warnings.push(w);
        }
    }
    warnings
}
