//! Directory renames and the barrel (`index.ts` `export *`) edits that follow them.
//!
//! When renamed files move from one directory to another whose name differs
//! (`layout/AccountMenuSiteProgramPicker/` → `layout/AccountMenuSitePicker/`), the directory
//! was renamed. Files too small for git's rename detection (the directory's own `index.ts`)
//! show up as a deletion in the old directory and an addition in the new one; they are paired
//! by file name here. Then every `export … from "<spec>"` edit that only points a re-export at
//! the renamed path (in the directory's index or in a parent barrel) is re-signed as the
//! identifier rename `rename\0{old}\0{new}` with detail `barrel`, so it joins the rename group
//! for the same pair, and tagged [`UnitTag::FileMove`]. A deletion and an addition in separate
//! units pair up when they re-export the same thing from the same (renamed) barrel.
//!
//! `export … from` is not an import site, so statements are matched by their tokens.
//!
//! Owned by workstream C2 (see docs/concept-lens-plan.md).

use std::collections::{BTreeMap, BTreeSet};

use indexmap::IndexMap;

use crate::cascade::{code_tokens, new_range, old_range, split_statements};
use crate::lang::{FileAnalysis, Lang, Token, TokenKind};
use crate::model::{FileStatus, FileSummary, Signature, SignatureKind, Unit, UnitTag};
use crate::moves::{Analyses, module_matches};

/// One re-export: the statement's tokens without the module, and the module.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReExport {
    shape: Vec<String>,
    spec: String,
}

/// A one-sided re-export edit waiting for its other half.
struct Half {
    unit_id: String,
    /// The importing file's path after the change.
    importer: String,
    pair: usize,
    export: ReExport,
}

/// Re-sign barrel edits that follow a directory rename.
pub fn link(units: &mut IndexMap<String, Unit>, files: &[FileSummary], analyses: &Analyses) {
    let pairs = path_pairs(files);
    if pairs.is_empty() {
        return;
    }
    // Where each file lived before and lives after, counting the paired add/delete as a move.
    let mut old_of: BTreeMap<&str, &str> = BTreeMap::new();
    let mut new_of: BTreeMap<&str, &str> = BTreeMap::new();
    for (o, n) in &pairs {
        old_of.insert(n, o);
        new_of.insert(o, n);
    }
    let old_path = |p: &str| old_of.get(p).copied().unwrap_or(p).to_string();
    let new_path = |p: &str| new_of.get(p).copied().unwrap_or(p).to_string();
    let pair_of = |spec: &str, importer: &str, old_side: bool| {
        pairs
            .iter()
            .position(|(po, pn)| module_matches(spec, if old_side { po } else { pn }, importer))
    };

    let mut resign: Vec<(String, Signature)> = Vec::new();
    let mut deleted: Vec<Half> = Vec::new();
    let mut added: Vec<Half> = Vec::new();
    for u in units.values() {
        if Lang::for_path(&u.path).is_none_or(Lang::is_python) {
            continue;
        }
        let Some((old_an, new_an)) = analyses.get(&u.path) else {
            continue;
        };
        let (importer_old, importer_new) = (old_path(&u.path), new_path(&u.path));
        let olds = old_range(u).map(|r| re_exports(old_an, r));
        let news = new_range(u).map(|r| re_exports(new_an, r));
        match (olds, news) {
            (Some(Some(olds)), Some(Some(news))) if olds.len() == news.len() => {
                let sigs: Vec<Signature> = olds
                    .iter()
                    .zip(&news)
                    .map_while(|(o, n)| {
                        let pi = pair_of(&o.spec, &importer_old, true)?;
                        (pair_of(&n.spec, &importer_new, false) == Some(pi)
                            && renamed_shape(o, n) == n.shape)
                            .then(|| signature(o, n))
                            .flatten()
                    })
                    .collect();
                if sigs.len() == olds.len() {
                    resign.extend(sigs.into_iter().map(|s| (u.id.clone(), s)));
                }
            }
            (Some(Some(mut olds)), None) if olds.len() == 1 => {
                if let Some(pair) = pair_of(&olds[0].spec, &importer_old, true) {
                    deleted.push(Half {
                        unit_id: u.id.clone(),
                        importer: importer_new,
                        pair,
                        export: olds.remove(0),
                    });
                }
            }
            (None, Some(Some(mut news))) if news.len() == 1 => {
                if let Some(pair) = pair_of(&news[0].spec, &importer_new, false) {
                    added.push(Half {
                        unit_id: u.id.clone(),
                        importer: importer_new,
                        pair,
                        export: news.remove(0),
                    });
                }
            }
            _ => {}
        }
    }
    // Pair one-sided halves: the deletion must read like the addition once renamed.
    let mut taken = vec![false; added.len()];
    for d in &deleted {
        let found = added.iter().enumerate().find(|(i, a)| {
            !taken[*i]
                && a.importer == d.importer
                && a.pair == d.pair
                && renamed_shape(&d.export, &a.export) == a.export.shape
        });
        let Some((i, a)) = found else {
            continue;
        };
        if let Some(s) = signature(&d.export, &a.export) {
            taken[i] = true;
            resign.push((d.unit_id.clone(), s.clone()));
            resign.push((a.unit_id.clone(), s));
        }
    }
    let mut cleared: BTreeSet<String> = BTreeSet::new();
    for (id, sig) in resign {
        let u = &mut units[&id];
        if cleared.insert(id.clone()) {
            u.signatures.clear();
        }
        if u.signatures.iter().all(|s| s.key != sig.key) {
            u.signatures.push(sig);
        }
        u.add_tag(UnitTag::FileMove);
    }
}

/// `(old path, new path)` for renamed files that changed directory name, plus deleted/added
/// files that sit in such a directory pair under the same (renamed) file name.
fn path_pairs(files: &[FileSummary]) -> Vec<(String, String)> {
    let parent = |p: &str| p.rsplit_once('/').map_or("", |(d, _)| d).to_string();
    let base = |p: &str| p.rsplit_once('/').map_or(p, |(_, b)| b).to_string();
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut dirs: BTreeSet<(String, String)> = BTreeSet::new();
    for f in files {
        let (FileStatus::Renamed, Some(old)) = (f.status, f.old_path.as_deref()) else {
            continue;
        };
        let (od, nd) = (parent(old), parent(&f.path));
        if base(&od) != base(&nd) {
            dirs.insert((od, nd));
            pairs.push((old.to_string(), f.path.clone()));
        }
    }
    for (od, nd) in &dirs {
        let (on, nn) = (base(od), base(nd));
        for d in files
            .iter()
            .filter(|f| f.status == FileStatus::Deleted && parent(&f.path) == *od)
        {
            let want = format!("{nd}/{}", base(&d.path).replace(&on, &nn));
            if files
                .iter()
                .any(|a| a.status == FileStatus::Added && a.path == want)
            {
                pairs.push((d.path.clone(), want));
            }
        }
    }
    pairs
}

/// The re-exports on `rng`, when the lines hold nothing else.
fn re_exports(an: &FileAnalysis, rng: (u32, u32)) -> Option<Vec<ReExport>> {
    let stmts = split_statements(&code_tokens(an, Some(rng)))?;
    if stmts.is_empty() {
        return None;
    }
    stmts.iter().map(|s| re_export(s)).collect()
}

/// `export … from "<spec>"`.
fn re_export(s: &[&Token]) -> Option<ReExport> {
    let (last, rest) = s.split_last()?;
    if s.first()?.value != "export"
        || last.kind != TokenKind::String
        || rest.last()?.value != "from"
    {
        return None;
    }
    Some(ReExport {
        shape: rest.iter().map(|t| t.value.clone()).collect(),
        spec: unquote(&last.text),
    })
}

fn unquote(text: &str) -> String {
    text.trim_matches(|c| matches!(c, '"' | '\'' | '`'))
        .to_string()
}

/// The last path component of a specifier, without a script extension.
fn last_segment(spec: &str) -> &str {
    let seg = spec.rsplit('/').next().unwrap_or(spec);
    seg.rsplit_once('.')
        .filter(|(_, ext)| matches!(*ext, "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs"))
        .map_or(seg, |(stem, _)| stem)
}

/// `old`'s shape with the renamed segment swapped in (`export { OldName } from` re-exports
/// follow the rename too).
fn renamed_shape(old: &ReExport, new: &ReExport) -> Vec<String> {
    let (o, n) = (last_segment(&old.spec), last_segment(&new.spec));
    old.shape
        .iter()
        .map(|v| if v == o { n.to_string() } else { v.clone() })
        .collect()
}

/// The rename signature for a re-export pointed at the renamed path.
fn signature(old: &ReExport, new: &ReExport) -> Option<Signature> {
    let (o, n) = (last_segment(&old.spec), last_segment(&new.spec));
    (o != n && !o.is_empty() && !n.is_empty()).then(|| {
        Signature::new(SignatureKind::Rename, format!("rename\0{o}\0{n}"), o, n)
            .with_detail("barrel")
    })
}
