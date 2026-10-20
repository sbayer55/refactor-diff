//! Tell renames apart from substitutions: `a → b` where `b` already existed in the base is one
//! concept standing in for another, not a rename. Re-signs such units as
//! [`SignatureKind::Substitution`](crate::model::SignatureKind::Substitution).
//!
//! * a `rename` `a → b` whose `b` is a name in the base of any analyzed file;
//!
//! The file's own base is checked first, the union over all files only when that misses.
//! * a `replace` of one string literal by another, `"x" → "y"`, whose `"y"` is a string in the
//!   base of any analyzed file (fixture ids such as `"site-001"` usually live in a shared
//!   mock module, not in the file that switches to them).
//!
//! An occurrence in the base only counts when the diff keeps it: one on a line no unit
//! touches, or in a unit whose new side still has it and that is not itself making this edit
//! (`def b` replaced by `def a` renamed to `b`). A `b` that only lived on lines the diff
//! deletes (the old definition removed to make room for the rename) leaves a real rename.
//!
//! Owned by workstream B (see docs/concept-lens-plan.md).

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;

use crate::lang::TokenKind;
use crate::model::{Signature, SignatureKind, Unit};
use crate::moves::Analyses;

/// Re-sign the renames and string replacements whose new side already existed in the base.
pub fn mark(units: &mut IndexMap<String, Unit>, analyses: &Analyses) {
    let mut resigned: Vec<(usize, usize, Signature)> = Vec::new();
    {
        let mut index = BaseIndex::new(units, analyses);
        for (ui, unit) in units.values().enumerate() {
            for (si, sig) in unit.signatures.iter().enumerate() {
                let found = match sig.kind {
                    SignatureKind::Rename => rename_target(&sig.key)
                        .filter(|b| index.has(&unit.path, b, &sig.key))
                        .map(|_| substitution(sig, &sig.key["rename\0".len()..])),
                    SignatureKind::Replace => string_target(&sig.key)
                        .filter(|(_, y)| index.has(&unit.path, y, &sig.key))
                        .map(|(x, y)| substitution(sig, &format!("{x}\0{y}"))),
                    _ => None,
                };
                if let Some(s) = found {
                    resigned.push((ui, si, s));
                }
            }
        }
    }
    // Back to front, so a removal never shifts a signature still to be replaced.
    for (ui, si, sig) in resigned.into_iter().rev() {
        let unit = &mut units[ui];
        if unit.signatures.iter().any(|s| s.key == sig.key) {
            unit.signatures.remove(si); // keep one signature per key, as classify does
        } else {
            unit.signatures[si] = sig;
        }
    }
}

fn substitution(sig: &Signature, pair: &str) -> Signature {
    Signature::new(
        SignatureKind::Substitution,
        format!("substitution\0{pair}"),
        &sig.old,
        &sig.new,
    )
    .with_detail(&sig.detail)
}

/// `b` of a `rename\0a\0b` key.
fn rename_target(key: &str) -> Option<&str> {
    let mut parts = key.split('\0');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("rename"), Some(_), Some(b), None) if !b.is_empty() => Some(b),
        _ => None,
    }
}

/// `("x", "y")` of a replace key that swaps one string literal for another.
fn string_target(key: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = key.split('\0').collect();
    match parts[..] {
        ["replace", x, "\x01", y] if is_string(x) && is_string(y) && x != y => Some((x, y)),
        _ => None,
    }
}

fn is_string(value: &str) -> bool {
    value.len() >= 2 && value.starts_with(['"', '\'', '`'])
}

/// The names and string literals in the base, per file and across all files, built on demand.
struct BaseIndex<'a> {
    analyses: &'a Analyses,
    /// Unit indices per path.
    by_path: HashMap<&'a str, Vec<usize>>,
    units: &'a IndexMap<String, Unit>,
    files: HashMap<&'a str, BaseValues<'a>>,
    global: Option<BaseValues<'a>>,
}

/// Token values (names and strings) of one base, split by whether the diff can drop them.
#[derive(Default)]
struct BaseValues<'a> {
    /// On lines no unit touches: certainly still there.
    kept: HashSet<&'a str>,
    /// On a unit's old lines: still there only when that unit's new side has it too and the
    /// unit is not the edit being tested.
    touched: HashMap<&'a str, Vec<usize>>,
}

impl<'a> BaseValues<'a> {
    fn has(
        &self,
        value: &str,
        key: &str,
        units: &IndexMap<String, Unit>,
        analyses: &Analyses,
    ) -> bool {
        if self.kept.contains(value) {
            return true;
        }
        self.touched.get(value).is_some_and(|us| {
            us.iter().any(|&ui| {
                let u = &units[ui];
                u.signatures.iter().all(|s| s.key != key) && new_side_has(u, value, analyses)
            })
        })
    }

    fn merge(&mut self, other: &BaseValues<'a>) {
        self.kept.extend(other.kept.iter().copied());
        for (v, us) in &other.touched {
            self.touched.entry(v).or_default().extend(us);
        }
    }
}

/// Whether `value` is a token on the new side of `unit`.
fn new_side_has(unit: &Unit, value: &str, analyses: &Analyses) -> bool {
    let Some((_, new_an)) = analyses.get(&unit.path) else {
        return false;
    };
    if unit.new.is_empty() {
        return false;
    }
    let (first, last) = (unit.new_start, unit.new_start + unit.new.len() as u32 - 1);
    new_an
        .tokens
        .iter()
        .any(|t| t.start.line >= first && t.start.line <= last && t.value == value)
}

impl<'a> BaseIndex<'a> {
    fn new(units: &'a IndexMap<String, Unit>, analyses: &'a Analyses) -> Self {
        let mut by_path: HashMap<&str, Vec<usize>> = HashMap::new();
        for (ui, u) in units.values().enumerate() {
            by_path.entry(u.path.as_str()).or_default().push(ui);
        }
        Self {
            analyses,
            by_path,
            units,
            files: HashMap::new(),
            global: None,
        }
    }

    /// Whether `value` survives in the base of `path`, or else of any analyzed file.
    /// `key` is the signature being tested: a unit carrying it does not vouch for `value`.
    fn has(&mut self, path: &str, value: &str, key: &str) -> bool {
        let (units, analyses) = (self.units, self.analyses);
        self.file(path)
            .is_some_and(|f| f.has(value, key, units, analyses))
            || self.global().has(value, key, units, analyses)
    }

    fn file(&mut self, path: &str) -> Option<&BaseValues<'a>> {
        let (key, _) = self.analyses.get_key_value(path)?;
        if !self.files.contains_key(key.as_str()) {
            let values = self.build(key);
            self.files.insert(key.as_str(), values);
        }
        self.files.get(key.as_str())
    }

    fn global(&mut self) -> &BaseValues<'a> {
        if self.global.is_none() {
            let mut all = BaseValues::default();
            let analyses = self.analyses;
            for path in analyses.keys() {
                if let Some(f) = self.file(path) {
                    all.merge(f);
                }
            }
            self.global = Some(all);
        }
        self.global.as_ref().expect("built above")
    }

    fn build(&self, path: &'a str) -> BaseValues<'a> {
        let (old_an, _) = &self.analyses[path];
        // Old line -> the units whose old side covers it.
        let mut line_units: HashMap<u32, Vec<usize>> = HashMap::new();
        for &ui in self
            .by_path
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let u = &self.units[ui];
            for line in u.old_start..u.old_start + u.old.len() as u32 {
                line_units.entry(line).or_default().push(ui);
            }
        }
        let mut values = BaseValues::default();
        for t in &old_an.tokens {
            if !matches!(t.kind, TokenKind::Name | TokenKind::String) {
                continue;
            }
            match line_units.get(&t.start.line) {
                None => {
                    values.kept.insert(t.value.as_str());
                }
                Some(us) => values
                    .touched
                    .entry(t.value.as_str())
                    .or_default()
                    .extend(us),
            }
        }
        values
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rename_and_string_keys() {
        assert_eq!(rename_target("rename\0a\0b"), Some("b"));
        assert_eq!(rename_target("replace\0a\0\x01\0b"), None);
        assert_eq!(
            string_target("replace\0\"x\"\0\x01\0\"y\""),
            Some(("\"x\"", "\"y\""))
        );
        assert_eq!(string_target("replace\0x\0\x01\0y"), None);
        assert_eq!(string_target("replace\0\"x\"\0,\0\x01\0\"y\""), None);
    }
}
