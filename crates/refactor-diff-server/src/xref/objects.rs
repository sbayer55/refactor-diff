//! `xref-storage-shape` and `xref-payload-drop`: object literals that are persisted
//! (`localStorage.setItem(key, JSON.stringify({...}))`) or sent (`fetch` bodies,
//! `api.post(url, {...})`, `const body = {...}`) and lose a field between base and head.
//!
//! Text-level and conservative: the literal must be written inline, objects are paired by
//! their storage key, URL or variable name, and a head object with a spread is never flagged.

use std::collections::{BTreeMap, HashSet};
use std::sync::LazyLock;

use refactor_diff_core::{Report, Severity};
use regex::Regex;

use super::{Check, PAYLOAD_DROP, STORAGE_SHAPE, changed_lines, is_js_like};

/// Bracket matching gives up past this many bytes.
const MAX_SPAN: usize = 20_000;

static STORAGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:localStorage|sessionStorage)\s*\.\s*setItem\s*\(").expect("valid regex")
});
static FETCH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bfetch\s*\(").expect("valid regex"));
static CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b([A-Za-z_$][\w$]*)\s*\.\s*(post|put|patch)\s*(?:<[^<>()]*>)?\s*\(")
        .expect("valid regex")
});
static VAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=;]+)?=\s*").expect("valid regex")
});
static BODY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bbody\s*:\s*").expect("valid regex"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Storage,
    Request,
}

#[derive(Debug)]
struct Obj {
    kind: Kind,
    anchor: String,
    /// 1-based lines of the call or declaration.
    first: u32,
    last: u32,
    keys: Vec<String>,
    spread: bool,
}

/// Byte offset → 1-based line.
struct Lines(Vec<usize>);

impl Lines {
    fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Lines(starts)
    }

    fn line(&self, offset: usize) -> u32 {
        self.0.partition_point(|&s| s <= offset) as u32
    }
}

/// Visit each code byte (outside strings and comments) from `start` to `end` with the
/// bracket depth before it; stop when `f` returns `true`. Returns the stopping offset.
fn walk(
    b: &[u8],
    start: usize,
    end: usize,
    mut f: impl FnMut(usize, i32) -> bool,
) -> Option<usize> {
    let end = end.min(b.len()).min(start + MAX_SPAN);
    let mut depth = 0;
    let mut i = start;
    while i < end {
        match b[i] {
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < end && b[i] != q && (q == b'`' || b[i] != b'\n') {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < end && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < end && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            c => {
                if matches!(c, b')' | b']' | b'}') {
                    depth -= 1;
                }
                if f(i, depth) {
                    return Some(i);
                }
                if matches!(c, b'(' | b'[' | b'{') {
                    depth += 1;
                }
            }
        }
        i += 1;
    }
    None
}

/// The offset of the bracket closing the one at `open`.
fn close_of(b: &[u8], open: usize) -> Option<usize> {
    walk(b, open, b.len(), |i, depth| {
        i > open && depth == 0 && matches!(b[i], b')' | b']' | b'}')
    })
}

/// Top-level comma-separated parts strictly inside the brackets at `open..=close`.
fn parts(b: &[u8], open: usize, close: usize) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut from = open + 1;
    walk(b, open + 1, close, |i, depth| {
        if depth == 0 && b[i] == b',' {
            out.push((from, i));
            from = i + 1;
        }
        false
    });
    out.push((from, close));
    out.into_iter()
        .map(|(s, e)| {
            let start = skip_ws(&b[..e], s);
            let trail = b[start..e]
                .iter()
                .rev()
                .take_while(|c| c.is_ascii_whitespace())
                .count();
            (start, e - trail)
        })
        .filter(|(s, e)| s < e)
        .collect()
}

/// Skip whitespace and comments.
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            while i < b.len() && !b[i..].starts_with(b"*/") {
                i += 1;
            }
            i = (i + 2).min(b.len());
        } else {
            return i;
        }
    }
}

/// The `{` of an object literal at `at` (after whitespace), or inside `JSON.stringify(` there
/// when `stringify` is allowed.
fn object_at(b: &[u8], at: usize, stringify: bool) -> Option<usize> {
    let i = skip_ws(b, at);
    if b.get(i) == Some(&b'{') {
        return Some(i);
    }
    let prefix = b"JSON.stringify";
    if stringify && b[i..].starts_with(prefix) {
        let j = skip_ws(b, i + prefix.len());
        if b.get(j) == Some(&b'(') {
            let k = skip_ws(b, j + 1);
            return (b.get(k) == Some(&b'{')).then_some(k);
        }
    }
    None
}

/// The keys of the object literal at `open`, and whether it has a spread.
fn keys_of(text: &str, open: usize) -> Option<(Vec<String>, bool, usize)> {
    let b = text.as_bytes();
    let close = close_of(b, open)?;
    let mut keys = vec![];
    let mut spread = false;
    for (s, e) in parts(b, open, close) {
        let entry = &text[s..e];
        if entry.starts_with("...") {
            spread = true;
            continue;
        }
        let key = if let Some(q) = entry.chars().next().filter(|c| matches!(c, '"' | '\'')) {
            entry[1..].split(q).next().unwrap_or("").to_string()
        } else {
            let ident: String = entry
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            let rest = entry[ident.len()..].trim_start();
            if ident.is_empty() || !(rest.is_empty() || rest.starts_with([':', '('])) {
                continue;
            }
            ident
        };
        if !key.is_empty() && !keys.contains(&key) {
            keys.push(key);
        }
    }
    Some((keys, spread, close))
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Persisted and sent object literals in one file.
fn scan(text: &str) -> Vec<Obj> {
    let b = text.as_bytes();
    let lines = Lines::new(text);
    let mut out = vec![];
    let mut push = |kind, anchor: String, start: usize, open: usize, end: usize| {
        if let Some((keys, spread, close)) = keys_of(text, open) {
            out.push(Obj {
                kind,
                anchor,
                first: lines.line(start),
                last: lines.line(end.max(close)),
                keys,
                spread,
            });
        }
    };
    let call_args = |paren: usize| {
        let close = close_of(b, paren)?;
        Some((close, parts(b, paren, close)))
    };
    for m in STORAGE_RE.find_iter(text) {
        let Some((close, args)) = call_args(m.end() - 1) else {
            continue;
        };
        if let [key, value, ..] = args[..] {
            if let Some(open) = object_at(b, value.0, true) {
                let anchor = format!("storage {}", squash(&text[key.0..key.1]));
                push(Kind::Storage, anchor, m.start(), open, close);
            }
        }
    }
    for m in FETCH_RE.find_iter(text) {
        let Some((close, args)) = call_args(m.end() - 1) else {
            continue;
        };
        let Some(url) = args.first() else { continue };
        for &(s, e) in &args[1..] {
            if let Some(body) = BODY_RE.find(&text[s..e]) {
                if let Some(open) = object_at(b, s + body.end(), true) {
                    let anchor = format!("fetch {}", squash(&text[url.0..url.1]));
                    push(Kind::Request, anchor, m.start(), open, close);
                }
            }
        }
    }
    for c in CALL_RE.captures_iter(text) {
        let recv = c[1].to_ascii_lowercase();
        if !["api", "client", "http", "axios", "request", "service"]
            .iter()
            .any(|w| recv.contains(w))
        {
            continue;
        }
        let m = c.get(0).expect("match");
        let Some((close, args)) = call_args(m.end() - 1) else {
            continue;
        };
        if let [url, value, ..] = args[..] {
            if let Some(open) = object_at(b, value.0, true) {
                let anchor = format!("{}.{} {}", &c[1], &c[2], squash(&text[url.0..url.1]));
                push(Kind::Request, anchor, m.start(), open, close);
            }
        }
    }
    for c in VAR_RE.captures_iter(text) {
        let name = c[1].to_ascii_lowercase();
        if !["body", "payload", "request"]
            .iter()
            .any(|w| name.contains(w))
        {
            continue;
        }
        let m = c.get(0).expect("match");
        if let Some(open) = object_at(b, m.end(), false) {
            push(
                Kind::Request,
                format!("var {}", &c[1]),
                m.start(),
                open,
                open,
            );
        }
    }
    out
}

fn quoted(keys: &[&String]) -> String {
    keys.iter()
        .map(|k| format!("`{k}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn check(report: &Report) -> Vec<Check> {
    let changed = changed_lines(report);
    let mut out = vec![];
    for (path, (old, new)) in &report.texts {
        if !is_js_like(path) || old.is_empty() || new.is_empty() {
            continue;
        }
        let Some(lines) = changed.get(path.as_str()) else {
            continue;
        };
        let touched = |set: &HashSet<u32>, o: &Obj| (o.first..=o.last).any(|l| set.contains(&l));
        // Head objects by anchor, paired with base objects in order.
        let mut news: BTreeMap<String, Vec<Obj>> = BTreeMap::new();
        for o in scan(new) {
            news.entry(o.anchor.clone()).or_default().push(o);
        }
        let mut taken: BTreeMap<String, usize> = BTreeMap::new();
        for o in scan(old) {
            let n = taken.entry(o.anchor.clone()).or_insert(0);
            let Some(h) = news
                .get(o.anchor.as_str())
                .and_then(|v| v.get(*n))
                .filter(|h| h.kind == o.kind)
            else {
                continue;
            };
            *n += 1;
            if h.spread || !(touched(&lines.old, &o) || touched(&lines.new, h)) {
                continue;
            }
            let dropped: Vec<&String> = o.keys.iter().filter(|k| !h.keys.contains(k)).collect();
            if dropped.is_empty() {
                continue;
            }
            let check = match o.kind {
                Kind::Storage => Check {
                    code: STORAGE_SHAPE,
                    severity: Severity::Warning,
                    path: path.clone(),
                    line: h.first,
                    message: format!(
                        "persisted payload drops {}; old stored values still carry {}",
                        quoted(&dropped),
                        if dropped.len() == 1 { "it" } else { "them" }
                    ),
                },
                Kind::Request => Check {
                    code: PAYLOAD_DROP,
                    severity: Severity::Info,
                    path: path.clone(),
                    line: h.first,
                    message: format!(
                        "request body ({}) drops {} (base line {})",
                        o.anchor,
                        quoted(&dropped),
                        o.first
                    ),
                },
            };
            out.push(check);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_persisted_and_sent_objects() {
        let text = "localStorage.setItem('k', JSON.stringify({\n  programId,\n  name: n, // x\n  'q': [1, 2],\n}));\nawait api.post<Foo>(`/x`, { a: 1, ...rest });\nconst requestBody: Body = { b, c() {} };\nfetch(url, { method: 'POST', body: JSON.stringify({ d: \"}\" }) });\n";
        let got: Vec<(String, u32, u32, Vec<String>, bool)> = scan(text)
            .into_iter()
            .map(|o| (o.anchor, o.first, o.last, o.keys, o.spread))
            .collect();
        let want = [
            ("storage 'k'", 1, 5, vec!["programId", "name", "q"], false),
            ("fetch url", 8, 8, vec!["d"], false),
            ("api.post `/x`", 6, 6, vec!["a"], true),
            ("var requestBody", 7, 7, vec!["b", "c"], false),
        ]
        .map(|(a, f, l, k, s)| {
            let keys = k.into_iter().map(String::from).collect();
            (a.to_string(), f, l, keys, s)
        });
        assert_eq!(got, want);
    }
}
