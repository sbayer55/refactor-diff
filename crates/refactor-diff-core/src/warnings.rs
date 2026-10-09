//! Sanity checks on grouped units: inconsistent renames, near misses and leftovers.

use std::collections::{BTreeMap, HashMap};

use indexmap::IndexMap;

use crate::lang::{FileAnalysis, LanguageAnalyzer, TokenKind};
use crate::model::{Group, Location, NearMiss, SignatureKind, Unit, Warning, WarningKind};
use crate::seqmatch::SequenceMatcher;

pub const MAX_LOCATIONS: usize = 50;
const SYMBOL_CONTEXTS: &[&str] = &["definition", "import"];
/// Largest mechanical groups considered per signature kind.
const NEAR_GROUPS_PER_KIND: usize = 200;
const NEAR_PER_UNIT: usize = 2;

/// A symbol (definition or import) renamed to different names in different places.
///
/// Locals and keyword arguments are skipped: renaming `user_id` differently in unrelated
/// functions is normal.
pub fn inconsistent_renames(groups: &[Group]) -> Vec<Warning> {
    let mut targets: IndexMap<&str, Vec<&Group>> = IndexMap::new();
    for g in groups.iter().filter(|g| g.kind == SignatureKind::Rename) {
        targets.entry(&g.old).or_default().push(g);
    }
    let mut warnings = Vec::new();
    for (old, gs) in targets {
        let symbolic = gs.iter().any(|g| {
            g.details
                .keys()
                .any(|d| SYMBOL_CONTEXTS.contains(&d.as_str()))
        });
        if gs.len() > 1 && symbolic {
            let news: Vec<String> = gs
                .iter()
                .map(|g| format!("{} (×{})", g.new, g.unit_ids.len()))
                .collect();
            // Python's max() keeps the first of several maxima.
            let mut biggest = gs[0];
            for g in &gs[1..] {
                if g.unit_ids.len() > biggest.unit_ids.len() {
                    biggest = g;
                }
            }
            warnings.push(Warning {
                kind: WarningKind::InconsistentRename,
                message: format!("{old} was renamed to different names: {}", news.join(", ")),
                group_id: Some(biggest.id.clone()),
                locations: vec![],
                total: 0,
            });
        }
    }
    warnings
}

/// Flag leftover changes that almost match a mechanical pattern: `get_user → fetch_users` next
/// to forty `get_user → fetch_user`, or a template differing in one token. Those are where
/// typos hide. Fills `unit.near` and returns one warning per pattern with near misses.
pub fn near_misses(units: &mut IndexMap<String, Unit>, groups: &[Group]) -> Vec<Warning> {
    let mut sorted: Vec<&Group> = groups.iter().collect();
    sorted.sort_by_key(|g| std::cmp::Reverse(g.unit_ids.len()));
    let mut by_kind: HashMap<SignatureKind, Vec<(&Group, Parts)>> = HashMap::new();
    for g in sorted {
        if g.mechanical && !g.kind.always_mechanical() {
            let entry = by_kind.entry(g.kind).or_default();
            if entry.len() < NEAR_GROUPS_PER_KIND {
                entry.push((g, sig_parts(g.kind, &g.key, &g.old, &g.new)));
            }
        }
    }

    let mut hits: IndexMap<String, Vec<String>> = IndexMap::new();
    for u in units.values_mut() {
        if u.explained {
            continue;
        }
        let mut found: IndexMap<&str, NearMiss> = IndexMap::new();
        for sig in &u.signatures {
            if sig.kind.always_mechanical() {
                continue;
            }
            let parts = sig_parts(sig.kind, &sig.key, &sig.old, &sig.new);
            for (g, g_parts) in by_kind.get(&sig.kind).map(Vec::as_slice).unwrap_or(&[]) {
                if g.key == sig.key {
                    continue;
                }
                let Some(score) = score(&parts, g_parts, sig.kind) else {
                    continue;
                };
                let side = if parts.0 == g_parts.0 { "new" } else { "old" };
                let differs = if side == "new" { &sig.new } else { &sig.old };
                let hint = format!("looks like `{}` ({side} side differs: {differs})", g.label);
                let better = found
                    .get(g.id.as_str())
                    .is_none_or(|best| score > best.score);
                if better {
                    found.insert(
                        &g.id,
                        NearMiss {
                            group_id: g.id.clone(),
                            score: round3(score),
                            hint,
                        },
                    );
                }
            }
        }
        let mut near: Vec<NearMiss> = found.into_values().collect();
        near.sort_by(|a, b| b.score.total_cmp(&a.score));
        near.truncate(NEAR_PER_UNIT);
        for n in &near {
            hits.entry(n.group_id.clone())
                .or_default()
                .push(u.id.clone());
        }
        u.near = near;
    }

    let by_id: HashMap<&str, &Group> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut warnings = Vec::new();
    for (gid, near_units) in hits {
        let g = by_id[gid.as_str()];
        let locations: Vec<Location> = near_units
            .iter()
            .take(MAX_LOCATIONS)
            .map(|uid| {
                let u = &units[uid];
                let line = if u.new.is_empty() {
                    u.old_start
                } else {
                    u.new_start
                };
                let text = if u.new.is_empty() { &u.old } else { &u.new }[0]
                    .text
                    .clone();
                Location {
                    path: u.path.clone(),
                    line,
                    text,
                }
            })
            .collect();
        let n = near_units.len();
        warnings.push(Warning {
            kind: WarningKind::NearMiss,
            message: format!(
                "{n} change{} look{} like a near miss of {}",
                if n != 1 { "s" } else { "" },
                if n != 1 { "" } else { "s" },
                g.label
            ),
            group_id: Some(gid),
            locations,
            total: n,
        });
    }
    warnings
}

/// `round(x, 3)`.
fn round3(score: f64) -> f64 {
    (score * 1000.0).round_ties_even() / 1000.0
}

type Parts = (String, String);

/// The two strings to compare per side: the callee and the shape delta for argument changes,
/// the displayed old/new text otherwise.
fn sig_parts(kind: SignatureKind, key: &str, old: &str, new: &str) -> Parts {
    if kind == SignatureKind::Args {
        let mut it = key.splitn(3, '\0');
        let _ = it.next();
        let name = it.next().unwrap_or("");
        let delta = it.next().unwrap_or("");
        return (name.to_string(), delta.to_string());
    }
    (old.to_string(), new.to_string())
}

fn sim(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    let (ca, cb): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (la, lb) = (ca.len() as f64, cb.len() as f64);
    if (la - lb).abs() > f64::max(3.0, 0.3 * la.max(lb)) {
        return 0.0;
    }
    let sm = SequenceMatcher::new(&ca, &cb);
    if sm.quick_ratio() >= 0.7 {
        sm.ratio()
    } else {
        0.0
    }
}

fn score(a: &Parts, b: &Parts, kind: SignatureKind) -> Option<f64> {
    let (so, sn) = (sim(&a.0, &b.0), sim(&a.1, &b.1));
    let ok = if kind == SignatureKind::Args {
        (so == 1.0 && sn >= 0.75) || (sn == 1.0 && so >= 0.85)
    } else {
        (so == 1.0 && sn >= 0.8) || (sn == 1.0 && so >= 0.8) || (so >= 0.9 && sn >= 0.9)
    };
    ok.then_some((so + sn) / 2.0)
}

/// Whether `name` is still defined here (def/class, or a top-level assignment).
pub fn still_defined(name: &str, analysis: &FileAnalysis, analyzer: &dyn LanguageAnalyzer) -> bool {
    let toks = &analysis.tokens;
    for (i, tok) in toks.iter().enumerate() {
        if tok.kind != TokenKind::Name || tok.value != name {
            continue;
        }
        if i > 0
            && analyzer
                .definition_keywords()
                .contains(&toks[i - 1].value.as_str())
        {
            return true;
        }
        if tok.start.col == 0
            && toks
                .get(i + 1)
                .is_some_and(|t| t.value == "=" || t.value == ":")
        {
            return true;
        }
    }
    false
}

/// One warning listing every identifier still spelled with the old name after a rename
/// (comments and strings don't count).
pub fn find_leftovers(group: &Group, files: &BTreeMap<String, &FileAnalysis>) -> Option<Warning> {
    let attribute_only = group.details.len() == 1 && group.details.contains_key("attribute");
    let mut locations: Vec<Location> = Vec::new();
    let mut total = 0usize;
    for (path, analysis) in files {
        let toks = &analysis.tokens;
        for (i, tok) in toks.iter().enumerate() {
            if tok.kind != TokenKind::Name || tok.value != group.old {
                continue;
            }
            if attribute_only && !(i > 0 && toks[i - 1].value == ".") {
                continue;
            }
            total += 1;
            let line = tok.start.line;
            let dup = locations
                .last()
                .is_some_and(|l| l.path == *path && l.line == line);
            if locations.len() < MAX_LOCATIONS && !dup {
                let text = analysis
                    .lines
                    .get(line as usize - 1)
                    .cloned()
                    .unwrap_or_default();
                locations.push(Location {
                    path: path.clone(),
                    line,
                    text,
                });
            }
        }
    }
    if total == 0 {
        return None;
    }
    let files_hit = locations
        .iter()
        .map(|l| &l.path)
        .collect::<std::collections::HashSet<_>>()
        .len();
    Some(Warning {
        kind: WarningKind::MissedRename,
        message: format!(
            "{} was renamed to {} and is no longer defined, but {total} reference{} remain{}",
            group.old,
            group.new,
            if total != 1 { "s" } else { "" },
            if files_hit > 1 {
                format!(" in {files_hit} files")
            } else {
                String::new()
            }
        ),
        group_id: Some(group.id.clone()),
        locations,
        total,
    })
}
