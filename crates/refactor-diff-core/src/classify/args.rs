//! Argument-list shape changes: ops that only add, remove or convert arguments of one call
//! (or parameters of one def) become an `args` signature keyed on the callee and the shape
//! change.

use std::collections::BTreeSet;

use indexmap::IndexMap;

use super::Side;
use crate::lang::{ArgKeyword, CallSite, DefSite, ParamKind, TokenKind};
use crate::model::{Signature, SignatureKind};
use crate::seqmatch::Opcode;
use crate::text::Pos;

/// A call argument or def parameter, normalized to how it is passed.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ArgItem {
    start: Pos,
    end: Pos,
    kind: ArgItemKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ArgItemKind {
    Pos,
    /// Keyword argument / parameter with a default or keyword-only, with its name.
    Kw(String),
    Star,
    StarStar,
}

/// A call or def that owns an argument list.
#[derive(Clone, Copy, Debug)]
enum Site<'a> {
    Call(&'a CallSite),
    Def(&'a DefSite),
}

impl<'a> Site<'a> {
    fn short_name(self) -> &'a str {
        match self {
            Site::Call(c) => c.short_name(),
            Site::Def(d) => d.short_name(),
        }
    }

    fn is_def(self) -> bool {
        matches!(self, Site::Def(_))
    }

    fn start(self) -> Pos {
        match self {
            Site::Call(c) => c.start,
            Site::Def(d) => d.start,
        }
    }

    fn end(self) -> Pos {
        match self {
            Site::Call(c) => c.end,
            Site::Def(d) => d.end,
        }
    }

    /// The argument list span, parens included.
    fn arg_span(self) -> (Pos, Pos) {
        match self {
            Site::Call(c) => (c.args_start, c.args_end),
            Site::Def(d) => (d.params_start, d.params_end),
        }
    }

    fn args(self) -> Vec<ArgItem> {
        match self {
            Site::Def(d) => d
                .params
                .iter()
                .map(|p| ArgItem {
                    start: p.start,
                    end: p.end,
                    kind: match p.kind {
                        ParamKind::VarArg => ArgItemKind::Star,
                        ParamKind::KwArg => ArgItemKind::StarStar,
                        ParamKind::KwOnly => ArgItemKind::Kw(p.name.clone()),
                        ParamKind::Pos | ParamKind::PosOnly if p.has_default => {
                            ArgItemKind::Kw(p.name.clone())
                        }
                        ParamKind::Pos | ParamKind::PosOnly => ArgItemKind::Pos,
                    },
                })
                .collect(),
            Site::Call(c) => c
                .args
                .iter()
                .map(|a| ArgItem {
                    start: a.start,
                    end: a.end,
                    kind: match &a.keyword {
                        ArgKeyword::Positional => ArgItemKind::Pos,
                        ArgKeyword::Named(name) => ArgItemKind::Kw(name.clone()),
                        ArgKeyword::Star => ArgItemKind::Star,
                        ArgKeyword::StarStar => ArgItemKind::StarStar,
                    },
                })
                .collect(),
        }
    }

    /// Python's `(end.line - start.line, end.col - start.col)`: the key that picks the
    /// innermost site.
    fn size(self) -> (i64, i64) {
        let (s, e) = (self.start(), self.end());
        (
            i64::from(e.line) - i64::from(s.line),
            i64::from(e.col) - i64::from(s.col),
        )
    }
}

/// Calls then defs, each with its index in that order (the pair-grouping key).
fn sites<'a>(side: &Side<'a>) -> Vec<Site<'a>> {
    let an = side.analysis;
    an.calls
        .iter()
        .map(Site::Call)
        .chain(an.defs.iter().map(Site::Def))
        .collect()
}

/// Explain the still-unclassified ops that only add, remove or convert arguments of one call
/// (or parameters of one def) with an `args` signature keyed on the callee and the shape
/// change, so `fetch(x, y)` -> `fetch(x, y, timeout=5)` groups with every other call that
/// gained `timeout` whatever its value, and with the def that gained the parameter.
pub(super) fn classify_args(
    a: &Side<'_>,
    b: &Side<'_>,
    ops: &[Opcode],
    classified: &mut [Option<Signature>],
    old_anchor: u32,
    new_anchor: u32,
) {
    let pending: Vec<usize> = (0..classified.len())
        .filter(|&k| classified[k].is_none())
        .collect();
    if pending.is_empty() {
        return;
    }
    let (a_sites, b_sites) = (sites(a), sites(b));
    let mut by_pair: IndexMap<(usize, usize), Vec<usize>> = IndexMap::new();
    for k in pending {
        let op = &ops[k];
        let mut new_site = site_for(b, &b_sites, op.j1, op.j2, None, new_anchor);
        let old_site = site_for(
            a,
            &a_sites,
            op.i1,
            op.i2,
            new_site.map(|(_, s)| s),
            old_anchor,
        );
        if new_site.is_none() {
            if let Some((_, o)) = old_site {
                new_site = site_for(b, &b_sites, op.j1, op.j2, Some(o), new_anchor);
            }
        }
        let (Some((oi, old_site)), Some((ni, new_site))) = (old_site, new_site) else {
            continue;
        };
        if old_site.short_name() != new_site.short_name() || old_site.is_def() != new_site.is_def()
        {
            continue;
        }
        by_pair.entry((oi, ni)).or_default().push(k);
    }
    for (&(oi, ni), ks) in &by_pair {
        if let Some(sig) = args_sig(a, b, ops, ks, a_sites[oi], b_sites[ni]) {
            for &k in ks {
                classified[k] = Some(sig.clone());
            }
        }
    }
}

/// The innermost call/def whose argument list holds the unit tokens `lo..hi`. For an empty
/// token run, the one around the insertion point; with no tokens at all on this side, the one
/// on the anchor line named like `other`.
fn site_for<'a>(
    side: &Side<'_>,
    sites: &[Site<'a>],
    lo: usize,
    hi: usize,
    other: Option<Site<'_>>,
    anchor: u32,
) -> Option<(usize, Site<'a>)> {
    let unit = side.tokens();
    let toks = &unit[lo..hi];
    let here: Vec<(usize, Site<'a>)> = if !toks.is_empty() {
        sites
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, s)| {
                let (lo, hi) = s.arg_span();
                toks.iter().all(|t| lo <= t.start && t.end <= hi)
            })
            .collect()
    } else if !unit.is_empty() {
        let p = if lo > 0 {
            unit[lo - 1].end
        } else {
            unit[lo.min(unit.len() - 1)].start
        };
        sites
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, s)| {
                let (lo, hi) = s.arg_span();
                lo <= p && p <= hi
            })
            .filter(|(_, s)| other.is_none_or(|o| s.short_name() == o.short_name()))
            .collect()
    } else {
        let other = other?;
        sites
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, s)| {
                s.short_name() == other.short_name()
                    && s.start().line <= anchor + 1
                    && s.end().line >= anchor
            })
            .collect()
    };
    // `min_by_key` keeps the first of equal keys, like Python's `min`.
    here.into_iter().min_by_key(|(_, s)| s.size())
}

fn args_sig(
    a: &Side<'_>,
    b: &Side<'_>,
    ops: &[Opcode],
    ks: &[usize],
    old_site: Site<'_>,
    new_site: Site<'_>,
) -> Option<Signature> {
    let mut cov_old: BTreeSet<usize> = BTreeSet::new();
    let mut cov_new: BTreeSet<usize> = BTreeSet::new();
    for &k in ks {
        let op = &ops[k];
        cov_old.extend(a.offset + op.i1..a.offset + op.i2);
        cov_new.extend(b.offset + op.j1..b.offset + op.j2);
    }

    let mut delta: Vec<String> = Vec::new();
    let mut conversions: Vec<String> = Vec::new();
    scan(a, old_site, &cov_old, '-', &mut delta, &mut conversions)?;
    scan(b, new_site, &cov_new, '+', &mut delta, &mut conversions)?;

    let positional = |site: Site<'_>| {
        site.args()
            .iter()
            .filter(|x| x.kind == ArgItemKind::Pos)
            .count() as i64
    };
    let (old_pos, new_pos) = (positional(old_site), positional(new_site));
    let has = |delta: &[String], d: &str| delta.iter().any(|x| x == d);
    if !conversions.is_empty() {
        if conversions.len() != 1 || old_pos - new_pos != 1 || has(&delta, "-pos") {
            return None;
        }
        delta.push(format!("pos>kw:{}", conversions[0]));
    }
    if delta.is_empty() || (has(&delta, "+pos") && has(&delta, "-pos")) {
        return None; // a positional argument replaced by another is a value edit
    }

    let mut counts: IndexMap<&str, usize> = IndexMap::new();
    for d in &delta {
        *counts.entry(d.as_str()).or_default() += 1;
    }
    let mut parts: Vec<String> = counts
        .iter()
        .map(|(&d, &n)| {
            if (d == "+pos" || d == "-pos") && n > 1 {
                format!("{d}:{n}")
            } else {
                d.to_string()
            }
        })
        .collect();
    parts.sort_unstable();
    let name = old_site.short_name();
    let is_def = old_site.is_def();
    let key = format!("args\0{name}\0{}", parts.join(","));

    let render_side = |sign: char| -> String {
        let mut extra: Vec<String> = Vec::new();
        for d in &parts {
            if let Some(kw) = d.strip_prefix(sign).and_then(|r| r.strip_prefix("kw:")) {
                extra.push(format!("{kw}=…"));
            } else if let Some(rest) = d.strip_prefix(sign).and_then(|r| r.strip_prefix("pos")) {
                let n: usize = rest.strip_prefix(':').map_or(1, |n| n.parse().unwrap_or(1));
                extra.extend(std::iter::repeat_n("…".to_string(), n));
            } else if d.strip_prefix(sign) == Some("*") || d.strip_prefix(sign) == Some("**") {
                extra.push(format!("{}…", &d[1..]));
            } else if let Some(kw) = d.strip_prefix("pos>kw:") {
                extra.push(if sign == '-' {
                    "…".to_string()
                } else {
                    format!("{kw}=…")
                });
            }
        }
        let mut inner = vec!["…".to_string()];
        inner.extend(extra);
        format!(
            "{}{name}({})",
            if is_def { "def " } else { "" },
            inner.join(", ")
        )
    };

    Some(
        Signature::new(SignatureKind::Args, key, render_side('-'), render_side('+'))
            .with_detail(if is_def { "definition" } else { "call" }),
    )
}

/// Check that the covered tokens of one side are whole arguments (or a positional-to-keyword
/// conversion, or separators/layout), recording the shape change in `delta`/`conversions`.
/// `None` means the edit touched the inside of an argument.
fn scan(
    side: &Side<'_>,
    site: Site<'_>,
    cov: &BTreeSet<usize>,
    sign: char,
    delta: &mut Vec<String>,
    conversions: &mut Vec<String>,
) -> Option<BTreeSet<usize>> {
    let mut allowed: BTreeSet<usize> = BTreeSet::new();
    let toks = &side.analysis.tokens;
    let (lo, hi) = site.arg_span();
    for item in site.args() {
        let idx: Vec<usize> = toks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.kind != TokenKind::Structural && item.start <= t.start && t.end <= item.end
            })
            .map(|(i, _)| i)
            .collect();
        let hit: Vec<usize> = idx.iter().copied().filter(|i| cov.contains(i)).collect();
        if hit.is_empty() {
            continue;
        }
        if hit.len() == idx.len() {
            allowed.extend(idx.iter().copied());
            delta.push(match &item.kind {
                ArgItemKind::Star => format!("{sign}*"),
                ArgItemKind::StarStar => format!("{sign}**"),
                ArgItemKind::Kw(name) => format!("{sign}kw:{name}"),
                ArgItemKind::Pos => format!("{sign}pos"),
            });
        } else {
            // A keyword argument whose value already existed: `f(x, 5)` -> `f(x, timeout=5)`
            // inserts only `timeout` and `=` in front of it.
            let converted = match &item.kind {
                ArgItemKind::Kw(name)
                    if sign == '+'
                        && hit.len() == 2
                        && hit[..] == idx[..2]
                        && toks[idx[1]].value == "=" =>
                {
                    Some(name)
                }
                _ => None,
            };
            let name = converted?;
            allowed.extend(hit.iter().copied());
            conversions.push(name.clone());
        }
    }
    for &i in cov {
        if allowed.contains(&i) {
            continue;
        }
        let t = &toks[i];
        let separator = t.is(TokenKind::Op, ",") && lo <= t.start && t.end <= hi;
        if separator || t.kind == TokenKind::Structural {
            allowed.insert(i);
        } else {
            return None;
        }
    }
    Some(allowed)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::*;
    use crate::lang::{Arg, FileAnalysis, Param};

    /// A single-line call site: `name(` at `open`, closing paren at `close`.
    fn call(name: &str, open: u32, close: u32, args: Vec<(u32, u32, ArgKeyword)>) -> CallSite {
        CallSite {
            name: name.into(),
            start: Pos::new(1, open - name.len() as u32),
            end: Pos::new(1, close + 1),
            args_start: Pos::new(1, open),
            args_end: Pos::new(1, close + 1),
            args: args
                .into_iter()
                .map(|(s, e, keyword)| Arg {
                    start: Pos::new(1, s),
                    end: Pos::new(1, e),
                    keyword,
                })
                .collect(),
        }
    }

    fn def(
        name: &str,
        open: u32,
        close: u32,
        params: Vec<(&str, ParamKind, bool, u32, u32)>,
    ) -> DefSite {
        DefSite {
            name: name.into(),
            start: Pos::new(1, 0),
            end: Pos::new(2, 8),
            params_start: Pos::new(1, open),
            params_end: Pos::new(1, close + 1),
            params: params
                .into_iter()
                .map(|(name, kind, has_default, s, e)| Param {
                    name: name.into(),
                    kind,
                    has_default,
                    start: Pos::new(1, s),
                    end: Pos::new(1, e),
                })
                .collect(),
        }
    }

    fn pos(s: u32, e: u32) -> (u32, u32, ArgKeyword) {
        (s, e, ArgKeyword::Positional)
    }

    fn kw(s: u32, e: u32, name: &str) -> (u32, u32, ArgKeyword) {
        (s, e, ArgKeyword::Named(name.into()))
    }

    fn keys(a: &FileAnalysis, b: &FileAnalysis) -> Vec<String> {
        let cls = super::super::classify(&TestLang, a, b, whole(a), whole(b), 0, 0);
        cls.signatures.iter().map(|s| s.key.clone()).collect()
    }

    #[test]
    fn added_keyword_argument_groups_across_values_and_with_the_def() {
        let mut a = analysis("fetch(1, 2)\n");
        a.calls = vec![call("fetch", 5, 10, vec![pos(6, 7), pos(9, 10)])];
        let mut b = analysis("fetch(1, 2, timeout=5)\n");
        b.calls = vec![call(
            "fetch",
            5,
            21,
            vec![pos(6, 7), pos(9, 10), kw(12, 21, "timeout")],
        )];
        let call1 = keys(&a, &b);

        let mut a2 = analysis("fetch(x, y)\n");
        a2.calls = vec![call("fetch", 5, 10, vec![pos(6, 7), pos(9, 10)])];
        let mut b2 = analysis("fetch(x, y, timeout=cfg.t)\n");
        b2.calls = vec![call(
            "fetch",
            5,
            25,
            vec![pos(6, 7), pos(9, 10), kw(12, 25, "timeout")],
        )];
        let call2 = keys(&a2, &b2);

        let mut a3 = analysis("def fetch(a, b):\n    pass\n");
        a3.defs = vec![def(
            "fetch",
            9,
            14,
            vec![
                ("a", ParamKind::Pos, false, 10, 11),
                ("b", ParamKind::Pos, false, 13, 14),
            ],
        )];
        let mut b3 = analysis("def fetch(a, b, timeout=None):\n    pass\n");
        b3.defs = vec![def(
            "fetch",
            9,
            28,
            vec![
                ("a", ParamKind::Pos, false, 10, 11),
                ("b", ParamKind::Pos, false, 13, 14),
                ("timeout", ParamKind::Pos, true, 16, 28),
            ],
        )];
        let definition = keys(&a3, &b3);
        assert_eq!(call1, vec!["args\0fetch\0+kw:timeout"]);
        assert_eq!(call1, call2);
        assert_eq!(call1, definition);

        let cls = super::super::classify(&TestLang, &a3, &b3, whole(&a3), whole(&b3), 0, 0);
        assert_eq!(
            sigs(&cls),
            vec![(
                "args",
                "def fetch(…)".into(),
                "def fetch(…, timeout=…)".into(),
                "definition".into()
            )]
        );
    }

    #[test]
    fn removed_keyword_and_positional_to_keyword() {
        let mut a = analysis("f(x, verbose=True)\n");
        a.calls = vec![call("f", 1, 17, vec![pos(2, 3), kw(5, 17, "verbose")])];
        let mut b = analysis("f(x)\n");
        b.calls = vec![call("f", 1, 3, vec![pos(2, 3)])];
        assert_eq!(keys(&a, &b), vec!["args\0f\0-kw:verbose"]);
        let cls = super::super::classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(cls.signatures[0].old, "f(…, verbose=…)");
        assert_eq!(cls.signatures[0].new, "f(…)");

        let mut a = analysis("f(x, 5)\n");
        a.calls = vec![call("f", 1, 6, vec![pos(2, 3), pos(5, 6)])];
        let mut b = analysis("f(x, timeout=5)\n");
        b.calls = vec![call("f", 1, 14, vec![pos(2, 3), kw(5, 14, "timeout")])];
        assert_eq!(keys(&a, &b), vec!["args\0f\0pos>kw:timeout"]);
        let cls = super::super::classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(cls.signatures[0].old, "f(…, …)");
        assert_eq!(cls.signatures[0].new, "f(…, timeout=…)");
    }

    #[test]
    fn edits_inside_arguments_are_not_args_changes() {
        // fetch(x, y) -> fetch(x, y + 1, timeout=5)
        let mut a = analysis("fetch(x, y)\n");
        a.calls = vec![call("fetch", 5, 10, vec![pos(6, 7), pos(9, 10)])];
        let mut b = analysis("fetch(x, y + 1, timeout=5)\n");
        b.calls = vec![call(
            "fetch",
            5,
            25,
            vec![pos(6, 7), pos(9, 14), kw(16, 25, "timeout")],
        )];
        assert!(keys(&a, &b).iter().all(|k| !k.starts_with("args")));
        // fetch(x, y) -> fetch(x, z, timeout=5): a replaced positional plus an added kwarg
        let mut b = analysis("fetch(x, z, timeout=5)\n");
        b.calls = vec![call(
            "fetch",
            5,
            21,
            vec![pos(6, 7), pos(9, 10), kw(12, 21, "timeout")],
        )];
        assert!(keys(&a, &b).iter().all(|k| !k.starts_with("args")));
    }

    #[test]
    fn positional_additions_count_and_star_args_render() {
        let mut a = analysis("f(1)\n");
        a.calls = vec![call("f", 1, 3, vec![pos(2, 3)])];
        let mut b = analysis("f(1, 2, 3)\n");
        b.calls = vec![call("f", 1, 9, vec![pos(2, 3), pos(5, 6), pos(8, 9)])];
        assert_eq!(keys(&a, &b), vec!["args\0f\0+pos:2"]);
        let cls = super::super::classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(cls.signatures[0].new, "f(…, …, …)");

        let mut b = analysis("f(1, **kw)\n");
        b.calls = vec![call(
            "f",
            1,
            9,
            vec![pos(2, 3), (5, 9, ArgKeyword::StarStar)],
        )];
        assert_eq!(keys(&a, &b), vec!["args\0f\0+**"]);
        let cls = super::super::classify(&TestLang, &a, &b, whole(&a), whole(&b), 0, 0);
        assert_eq!(cls.signatures[0].new, "f(…, **…)");
    }

    #[test]
    fn innermost_site_wins_and_method_calls_share_the_key() {
        let mut a = analysis("log(fetch(x))\n");
        a.calls = vec![
            call("log", 3, 12, vec![pos(4, 12)]),
            call("fetch", 9, 11, vec![pos(10, 11)]),
        ];
        let mut b = analysis("log(fetch(x, timeout=1))\n");
        b.calls = vec![
            call("log", 3, 23, vec![pos(4, 23)]),
            call("fetch", 9, 22, vec![pos(10, 11), kw(13, 22, "timeout")]),
        ];
        assert_eq!(keys(&a, &b), vec!["args\0fetch\0+kw:timeout"]);

        let mut a = analysis("client.fetch(x)\n");
        a.calls = vec![call("client.fetch", 12, 14, vec![pos(13, 14)])];
        let mut b = analysis("client.fetch(x, timeout=1)\n");
        b.calls = vec![call(
            "client.fetch",
            12,
            25,
            vec![pos(13, 14), kw(16, 25, "timeout")],
        )];
        assert_eq!(keys(&a, &b), vec!["args\0fetch\0+kw:timeout"]);
    }

    #[test]
    fn added_line_in_a_multiline_call_uses_the_anchor() {
        // r = fetch(\n    x,\n    timeout=5,\n)\n : the unit is the inserted line 3 only.
        let mut a = analysis("r = fetch(\n    x,\n)\n");
        a.calls = vec![CallSite {
            name: "fetch".into(),
            start: Pos::new(1, 4),
            end: Pos::new(3, 1),
            args_start: Pos::new(1, 9),
            args_end: Pos::new(3, 1),
            args: vec![Arg {
                start: Pos::new(2, 4),
                end: Pos::new(2, 5),
                keyword: ArgKeyword::Positional,
            }],
        }];
        let mut b = analysis("r = fetch(\n    x,\n    timeout=5,\n)\n");
        b.calls = vec![CallSite {
            name: "fetch".into(),
            start: Pos::new(1, 4),
            end: Pos::new(4, 1),
            args_start: Pos::new(1, 9),
            args_end: Pos::new(4, 1),
            args: vec![
                Arg {
                    start: Pos::new(2, 4),
                    end: Pos::new(2, 5),
                    keyword: ArgKeyword::Positional,
                },
                Arg {
                    start: Pos::new(3, 4),
                    end: Pos::new(3, 13),
                    keyword: ArgKeyword::Named("timeout".into()),
                },
            ],
        }];
        let cls = super::super::classify(&TestLang, &a, &b, None, Some((3, 3)), 2, 2);
        assert_eq!(
            cls.signatures
                .iter()
                .map(|s| s.key.as_str())
                .collect::<Vec<_>>(),
            vec!["args\0fetch\0+kw:timeout"]
        );
    }

    #[test]
    fn site_for_picks_the_innermost_match() {
        let mut an = analysis("log(fetch(x))\n");
        an.calls = vec![
            call("log", 3, 12, vec![pos(4, 12)]),
            call("fetch", 9, 11, vec![pos(10, 11)]),
        ];
        let side = tokens_in_range(&an, Some((1, 1)));
        let s = sites(&side);
        // token 4 is "x": inside both argument lists; the shorter call wins.
        let (i, site) = site_for(&side, &s, 4, 5, None, 0).unwrap();
        assert_eq!((i, site.short_name()), (1, "fetch"));
        // an empty run at the insertion point after "x" filters by the other side's name
        let other = Site::Call(&an.calls[0]);
        let (i, _) = site_for(&side, &s, 5, 5, Some(other), 0).unwrap();
        assert_eq!(i, 0);
        // the callee name is outside every argument list
        assert!(site_for(&side, &s, 0, 1, None, 0).is_none());
        // a zero-width structural token exactly at the closing paren's end counts as inside
        // (Python compares positions the same way)
        assert_eq!(site_for(&side, &s, 7, 8, None, 0).unwrap().0, 0);
    }

    #[test]
    fn separators_and_layout_in_the_cover_are_allowed_but_values_are_not() {
        let mut a = analysis("f(x,)\n");
        a.calls = vec![call("f", 1, 4, vec![pos(2, 3)])];
        let side = tokens_in_range(&a, whole(&a));
        let site = Site::Call(&a.calls[0]);
        // "," (index 3) and the "<NEWLINE>" (index 5) are allowed on their own
        let cov: BTreeSet<usize> = [3, 5].into_iter().collect();
        let (mut delta, mut conv) = (Vec::new(), Vec::new());
        assert_eq!(
            scan(&side, site, &cov, '-', &mut delta, &mut conv),
            Some(cov.clone())
        );
        assert!(delta.is_empty());
        // the closing paren is not
        let cov: BTreeSet<usize> = [4].into_iter().collect();
        assert!(scan(&side, site, &cov, '-', &mut delta, &mut conv).is_none());
        // the whole argument "x" is a removal
        let cov: BTreeSet<usize> = [2].into_iter().collect();
        assert!(scan(&side, site, &cov, '-', &mut delta, &mut conv).is_some());
        assert_eq!(delta, vec!["-pos"]);
    }

    fn tokens_in_range<'a>(an: &'a FileAnalysis, rng: super::super::LineRange) -> Side<'a> {
        super::super::tokens_in_range(an, rng)
    }
}
