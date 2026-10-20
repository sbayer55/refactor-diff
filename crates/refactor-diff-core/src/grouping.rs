//! Cluster classified units into groups.

use std::collections::{BTreeSet, HashSet};

use indexmap::IndexMap;

use crate::hash::short_hash;
use crate::model::{Group, Signature, Unit};

/// One group per signature key. A group is *mechanical* when it repeats at least `min_count`
/// times (formatting always is); a unit is *explained* when every signature it carries belongs
/// to a mechanical group.
pub fn build_groups(units: &mut IndexMap<String, Unit>, min_count: usize) -> Vec<Group> {
    // (unit index, signature index) per key, in first-seen order.
    let mut by_key: IndexMap<&str, Vec<(usize, usize)>> = IndexMap::new();
    for (ui, unit) in units.values().enumerate() {
        for (si, sig) in unit.signatures.iter().enumerate() {
            by_key.entry(&sig.key).or_default().push((ui, si));
        }
    }

    let mut groups = Vec::with_capacity(by_key.len());
    for (key, members) in &by_key {
        let (first_u, first_s) = members[0];
        let first = &units[first_u].signatures[first_s];
        // The most common (old, new) among the members speaks for the group; ties go to the
        // first seen.
        let mut variants: IndexMap<(&str, &str), (usize, &Signature)> = IndexMap::new();
        for &(ui, si) in members {
            let sig = &units[ui].signatures[si];
            variants
                .entry((sig.old.as_str(), sig.new.as_str()))
                .or_insert((0, sig))
                .0 += 1;
        }
        let mut rep = first;
        let mut best = 0;
        for &(count, sig) in variants.values() {
            if count > best {
                (best, rep) = (count, sig);
            }
        }
        let mut label = rep.label();
        if variants.len() > 1 {
            let n = variants.len() - 1;
            let s = if n == 1 { "" } else { "s" };
            label = format!("{label} (+{n} variant{s})");
        }
        let mut unit_ids: Vec<String> = Vec::new();
        let mut seen = HashSet::new();
        let mut files = BTreeSet::new();
        let mut details: IndexMap<String, usize> = IndexMap::new();
        for &(ui, si) in members {
            let unit = &units[ui];
            if seen.insert(ui) {
                unit_ids.push(unit.id.clone());
            }
            files.insert(unit.path.clone());
            let detail = &unit.signatures[si].detail;
            if !detail.is_empty() {
                *details.entry(detail.clone()).or_default() += 1;
            }
        }
        // Counter.most_common(): descending count, ties in first-seen order.
        let mut details: Vec<(String, usize)> = details.into_iter().collect();
        details.sort_by_key(|d| std::cmp::Reverse(d.1));
        let mechanical = !first.kind.never_mechanical()
            && (first.kind.always_mechanical() || unit_ids.len() >= min_count);
        groups.push(Group {
            id: short_hash([key]),
            key: key.to_string(),
            kind: rep.kind,
            label,
            old: rep.old.clone(),
            new: rep.new.clone(),
            unit_ids,
            files: files.into_iter().collect(),
            details: details.into_iter().collect(),
            mechanical,
        });
    }

    let mechanical: HashSet<&str> = groups
        .iter()
        .filter(|g| g.mechanical)
        .map(|g| g.key.as_str())
        .collect();
    for unit in units.values_mut() {
        unit.explained = !unit.signatures.is_empty()
            && unit
                .signatures
                .iter()
                .all(|s| mechanical.contains(s.key.as_str()));
    }

    groups.sort_by_key(|g| {
        (
            !g.mechanical,
            g.kind.always_mechanical(),
            std::cmp::Reverse(g.unit_ids.len()),
            g.label.clone(),
        )
    });
    groups
}
