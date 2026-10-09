//! Import-only units, classified by what their statements bind rather than token by token.

use super::{Classification, LineRange, Side, highlight, tokens_in_range};
use crate::lang::{Binding, FileAnalysis, ImportSite, LanguageAnalyzer, Token, TokenKind};
use crate::model::{Signature, SignatureKind};
use crate::seqmatch::Opcode;

/// A unit made only of import statements: classify it by what the imports bind.
///
/// * the same names from a different module -> `import` (`a` -> `b`), keyed on the module
///   change so the same move/rename of a module groups across files;
/// * a name added or removed -> `import` keyed on the binding;
/// * the same module importing a differently named thing -> a `rename` with the `import`
///   context, the same signature the token classifier would produce.
///
/// Determinism: Python pairs each removed binding with the first matching candidate of an
/// unordered set. Here `added` is kept sorted by `(alias, module, name)`, so the candidate
/// with the smallest module (for a module change) or name (for a rename) wins, and ties in
/// the removed order are broken by name too.
pub(super) fn import_classification(
    a: &Side<'_>,
    b: &Side<'_>,
    ops: &[Opcode],
    old_range: LineRange,
    new_range: LineRange,
) -> Option<Classification> {
    let old_sites = import_sites(a, old_range)?;
    let new_sites = import_sites(b, new_range)?;
    let old_b = bindings_of(&old_sites);
    let new_b = bindings_of(&new_sites);
    let mut removed: Vec<&Binding> = old_b
        .iter()
        .copied()
        .filter(|x| !new_b.contains(x))
        .collect();
    let mut added: Vec<&Binding> = new_b
        .iter()
        .copied()
        .filter(|x| !old_b.contains(x))
        .collect();
    if removed.is_empty() && added.is_empty() {
        return None;
    }
    removed.sort_by_key(|x| sort_key(x));
    added.sort_by_key(|x| sort_key(x));

    let mut sigs: Vec<Signature> = Vec::new();
    let mut unpaired: Vec<&Binding> = Vec::new();
    for r in removed {
        if let Some(i) = added
            .iter()
            .position(|x| x.name == r.name && x.alias == r.alias)
        {
            let n = added.remove(i);
            let key = format!("import\0{}\0{}", r.module, n.module);
            sigs.push(
                Signature::new(SignatureKind::Import, key, &r.module, &n.module)
                    .with_detail(&r.alias),
            );
            continue;
        }
        if let Some(i) = added.iter().position(|x| {
            x.module == r.module
                && named(x)
                && named(r)
                && x.name.as_deref() == Some(x.alias.as_str())
                && r.name.as_deref() == Some(r.alias.as_str())
        }) {
            let n = added.remove(i);
            let (old_name, new_name) = (name_of(r), name_of(n));
            let key = format!("rename\0{old_name}\0{new_name}");
            sigs.push(
                Signature::new(SignatureKind::Rename, key, old_name, new_name)
                    .with_detail("import"),
            );
            continue;
        }
        unpaired.push(r);
    }
    if !unpaired.is_empty() && !added.is_empty() {
        return None; // something else was rewritten: leave it to the token classifier
    }
    for r in unpaired {
        let key = format!("import\0{}\0", r.dotted());
        sigs.push(
            Signature::new(SignatureKind::Import, key, import_text(r), "").with_detail(&r.alias),
        );
    }
    for n in added {
        let key = format!("import\0\0{}", n.dotted());
        sigs.push(
            Signature::new(SignatureKind::Import, key, "", import_text(n)).with_detail(&n.alias),
        );
    }
    let mut result = Classification::default();
    for s in sigs {
        result.add(s);
    }
    for op in ops {
        highlight(&mut result.old_hl, &a.tokens()[op.i1..op.i2], old_range);
        highlight(&mut result.new_hl, &b.tokens()[op.j1..op.j2], new_range);
    }
    Some(result)
}

/// The distinct bindings of `sites`, in first-seen order (a set in Python).
fn bindings_of<'a>(sites: &[&'a ImportSite]) -> Vec<&'a Binding> {
    let mut out: Vec<&Binding> = Vec::new();
    for x in sites.iter().flat_map(|s| s.bindings.iter()) {
        if !out.contains(&x) {
            out.push(x);
        }
    }
    out
}

fn sort_key(b: &Binding) -> (&str, &str, Option<&str>) {
    (&b.alias, &b.module, b.name.as_deref())
}

/// Python's truthiness of `binding.name`: present and non-empty.
fn named(b: &Binding) -> bool {
    b.name.as_deref().is_some_and(|n| !n.is_empty())
}

fn name_of(b: &Binding) -> &str {
    b.name.as_deref().unwrap_or_default()
}

/// Whether all the code on both sides is import statements (and there is some code at all):
/// a change that only touches imports, whatever it did to them.
pub fn imports_only(
    analyzer: &dyn LanguageAnalyzer,
    old: &FileAnalysis,
    new: &FileAnalysis,
    old_range: LineRange,
    new_range: LineRange,
) -> bool {
    let mut code = 0usize;
    for (analysis, rng) in [(old, old_range), (new, new_range)] {
        let side = tokens_in_range(analysis, rng);
        if import_sites(&side, rng).is_none() {
            return false;
        }
        if !statements_start_with(&side, analyzer.import_keywords()) {
            return false; // e.g. ``import os; x = 1``
        }
        code += side
            .tokens()
            .iter()
            .filter(|t| !matches!(t.kind, TokenKind::Structural | TokenKind::Comment))
            .count();
    }
    code > 0
}

/// Whether every statement the side's tokens belong to starts with one of `keywords`
/// (statements that begin before the range included).
fn statements_start_with(side: &Side<'_>, keywords: &[&str]) -> bool {
    let toks = &side.analysis.tokens;
    let ends = |t: &Token| t.kind == TokenKind::Structural || t.is(TokenKind::Op, ";");
    for i in side.offset..side.offset + side.len {
        let t = &toks[i];
        if t.kind == TokenKind::Comment || ends(t) {
            continue;
        }
        if i > side.offset && !ends(&toks[i - 1]) && toks[i - 1].kind != TokenKind::Comment {
            continue; // same statement as the token before
        }
        let mut j = i;
        while j > 0 && !ends(&toks[j - 1]) {
            j -= 1;
        }
        let head = toks[j..=i]
            .iter()
            .find(|x| x.kind != TokenKind::Comment)
            .unwrap_or(t);
        if !keywords.contains(&head.value.as_str()) {
            return false;
        }
    }
    true
}

/// The import statements overlapping the range, or `None` if the range holds anything else
/// (an empty side counts as all imports).
fn import_sites<'a>(side: &Side<'a>, rng: LineRange) -> Option<Vec<&'a ImportSite>> {
    let Some((first, last)) = rng else {
        return Some(vec![]);
    };
    let sites: Vec<&ImportSite> = side
        .analysis
        .imports
        .iter()
        .filter(|s| s.start <= last && s.end >= first)
        .collect();
    for t in side.tokens() {
        if matches!(t.kind, TokenKind::Structural | TokenKind::Comment) {
            continue;
        }
        if !sites
            .iter()
            .any(|s| s.start <= t.start.line && t.start.line <= s.end)
        {
            return None;
        }
    }
    Some(sites)
}

/// The display form of a binding: its own `text`, or Python import syntax.
fn import_text(binding: &Binding) -> String {
    if !binding.text.is_empty() {
        return binding.text.clone();
    }
    let (text, alias) = match &binding.name {
        None => (
            format!("import {}", binding.module),
            binding.module.split('.').next().unwrap_or_default(),
        ),
        Some(name) => (
            format!("from {} import {}", binding.module, name),
            name.as_str(),
        ),
    };
    if binding.alias == alias {
        text
    } else {
        format!("{text} as {}", binding.alias)
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;

    fn site(start: u32, end: u32, bindings: Vec<Binding>) -> ImportSite {
        ImportSite {
            start,
            end,
            bindings,
        }
    }

    fn from(module: &str, name: &str) -> Binding {
        Binding::new(module, Some(name.into()), name)
    }

    fn import(module: &str) -> Binding {
        Binding::new(module, None, module.split('.').next().unwrap())
    }

    fn classify_imports(
        old: &str,
        old_sites: Vec<ImportSite>,
        new: &str,
        new_sites: Vec<ImportSite>,
    ) -> Option<Classification> {
        let (mut a, mut b) = (analysis(old), analysis(new));
        a.imports = old_sites;
        b.imports = new_sites;
        let sa = tokens_in_range(&a, whole(&a));
        let sb = tokens_in_range(&b, whole(&b));
        import_classification(&sa, &sb, &[], whole(&a), whole(&b))
    }

    #[test]
    fn import_text_formats_python_syntax_unless_text_is_set() {
        assert_eq!(import_text(&import("os.path")), "import os.path");
        assert_eq!(import_text(&from("a", "x")), "from a import x");
        assert_eq!(
            import_text(&Binding::new("a", Some("x".into()), "y")),
            "from a import x as y"
        );
        assert_eq!(
            import_text(&Binding::new("numpy", None, "np")),
            "import numpy as np"
        );
        let mut ts = Binding::new("./u", Some("getUser".into()), "getUser");
        ts.text = "import { getUser } from \"./u\"".into();
        assert_eq!(import_text(&ts), ts.text);
    }

    #[test]
    fn module_change_rename_and_additions() {
        let old = "from a import x\nfrom p import h\n";
        let new = "import logging\n\nfrom a import y\nfrom q import h\n";
        let cls = classify_imports(
            old,
            vec![
                site(1, 1, vec![from("a", "x")]),
                site(2, 2, vec![from("p", "h")]),
            ],
            new,
            vec![
                site(1, 1, vec![import("logging")]),
                site(3, 3, vec![from("a", "y")]),
                site(4, 4, vec![from("q", "h")]),
            ],
        )
        .unwrap();
        let found: std::collections::BTreeSet<_> = sigs(&cls).into_iter().collect();
        let expected: std::collections::BTreeSet<_> = [
            ("import", "p".to_string(), "q".to_string(), "h".to_string()),
            ("rename", "x".into(), "y".into(), "import".into()),
            (
                "import",
                "".into(),
                "import logging".into(),
                "logging".into(),
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(found, expected);
        let keys: Vec<&str> = cls.signatures.iter().map(|s| s.key.as_str()).collect();
        assert!(keys.contains(&"import\0p\0q"));
        assert!(keys.contains(&"rename\0x\0y"));
        assert!(keys.contains(&"import\0\0logging"));
    }

    #[test]
    fn removed_name_and_module_plus_name_change() {
        let cls = classify_imports(
            "from a import x, y\n",
            vec![site(1, 1, vec![from("a", "x"), from("a", "y")])],
            "from a import x\n",
            vec![site(1, 1, vec![from("a", "x")])],
        )
        .unwrap();
        assert_eq!(
            sigs(&cls),
            vec![("import", "from a import y".into(), "".into(), "y".into())]
        );
        assert_eq!(cls.signatures[0].key, "import\0a.y\0");
        // Module and name both changed: nothing pairs, so the token classifier takes over.
        assert!(
            classify_imports(
                "from a import x\n",
                vec![site(1, 1, vec![from("a", "x")])],
                "from b import y\n",
                vec![site(1, 1, vec![from("b", "y")])],
            )
            .is_none()
        );
        // Nothing changed in what is bound (e.g. only layout): not an import classification.
        assert!(
            classify_imports(
                "from a import x\n",
                vec![site(1, 1, vec![from("a", "x")])],
                "from a import (x)\n",
                vec![site(1, 1, vec![from("a", "x")])],
            )
            .is_none()
        );
    }

    #[test]
    fn empty_side_is_all_imports() {
        let cls = classify_imports(
            "",
            vec![],
            "from __future__ import annotations\n",
            vec![site(1, 1, vec![from("__future__", "annotations")])],
        )
        .unwrap();
        assert_eq!(cls.signatures[0].key, "import\0\0__future__.annotations");
    }

    #[test]
    fn pairing_is_deterministic_by_sorted_candidates() {
        // `x` moved from `a`; two candidates import `x` from different modules.
        let cls = classify_imports(
            "from a import x\n",
            vec![site(1, 1, vec![from("a", "x")])],
            "from z import x\nfrom b import x\n",
            vec![
                site(1, 1, vec![from("z", "x")]),
                site(2, 2, vec![from("b", "x")]),
            ],
        )
        .unwrap();
        let keys: Vec<&str> = cls.signatures.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["import\0a\0b", "import\0\0z.x"]);
    }

    #[test]
    fn code_outside_import_sites_disqualifies() {
        let mut an = analysis("from a import x\nz = 1\n");
        an.imports = vec![site(1, 1, vec![from("a", "x")])];
        let side = tokens_in_range(&an, Some((1, 2)));
        assert!(import_sites(&side, Some((1, 2))).is_none());
        let side = tokens_in_range(&an, Some((1, 1)));
        assert_eq!(import_sites(&side, Some((1, 1))).unwrap().len(), 1);
        // Comments are allowed anywhere.
        let mut an = analysis("# note\nfrom a import x\n");
        an.imports = vec![site(2, 2, vec![from("a", "x")])];
        let side = tokens_in_range(&an, Some((1, 2)));
        assert_eq!(import_sites(&side, Some((1, 2))).unwrap().len(), 1);
    }

    #[test]
    fn statements_start_with_keywords() {
        // Inside the parentheses Python emits no logical-line end, so drop the test
        // tokenizer's "<NEWLINE>" tokens on lines 2 and 3.
        let mut an = analysis("import os\nfrom a import (\n    x,\n)\n");
        an.tokens
            .retain(|t| !(t.kind == TokenKind::Structural && (2..=3).contains(&t.start.line)));
        let side = tokens_in_range(&an, Some((3, 3)));
        assert!(statements_start_with(&side, &["import", "from"]));
        let an = analysis("import os; x = 1\n");
        let side = tokens_in_range(&an, Some((1, 1)));
        assert!(!statements_start_with(&side, &["import", "from"]));
        let an = analysis("# c\nimport os  # trailing\n");
        let side = tokens_in_range(&an, Some((1, 2)));
        assert!(statements_start_with(&side, &["import", "from"]));
    }

    #[test]
    fn imports_only_needs_import_sites_and_some_code() {
        let mut a = analysis("import os\n");
        a.imports = vec![site(1, 1, vec![import("os")])];
        let mut b = analysis("import os\nimport sys\n");
        b.imports = vec![
            site(1, 1, vec![import("os")]),
            site(2, 2, vec![import("sys")]),
        ];
        assert!(imports_only(&TestLang, &a, &b, Some((1, 1)), Some((1, 2))));
        assert!(imports_only(&TestLang, &a, &b, None, Some((2, 2))));
        assert!(
            !imports_only(&TestLang, &a, &b, None, None),
            "no code at all"
        );
        let c = analysis("x = 1\n");
        assert!(!imports_only(&TestLang, &a, &c, Some((1, 1)), Some((1, 1))));
    }
}
