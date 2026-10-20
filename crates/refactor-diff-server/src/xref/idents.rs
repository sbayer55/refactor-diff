//! Identifier scanning and concept-stem matching.

/// Identifiers in `line` (JS-style: ASCII letters, digits, `_` and `$`, not starting with a
/// digit), as `(byte offset, identifier)`.
pub fn identifiers(line: &str) -> Vec<(usize, &str)> {
    let bytes = line.as_bytes();
    let is_part = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let mut out = vec![];
    let mut i = 0;
    while i < bytes.len() {
        if is_part(bytes[i]) {
            let start = i;
            while i < bytes.len() && is_part(bytes[i]) {
                i += 1;
            }
            if !bytes[start].is_ascii_digit() {
                out.push((start, &line[start..i]));
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The lowercase words of an identifier: split at `_`, `$`, `-`, lower→upper and letter↔digit
/// boundaries, and before the last capital of a capital run (`HTMLParser` → `html`, `parser`).
pub fn words(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut out = vec![];
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(&prev) = i.checked_sub(1).and_then(|p| chars.get(p)) {
            let next = chars.get(i + 1).copied();
            let boundary = (prev.is_ascii_lowercase() && c.is_ascii_uppercase())
                || (prev.is_ascii_digit() != c.is_ascii_digit() && prev.is_ascii_alphanumeric())
                || (prev.is_ascii_uppercase()
                    && c.is_ascii_uppercase()
                    && next.is_some_and(|n| n.is_ascii_lowercase()));
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A concept stem, split into words, matched against identifiers word by word.
#[derive(Clone, Debug)]
pub struct Stem {
    words: Vec<String>,
}

impl Stem {
    pub fn new(stem: &str) -> Option<Self> {
        let words = words(stem);
        (!words.is_empty()).then_some(Stem { words })
    }

    pub fn first_word(&self) -> &str {
        &self.words[0]
    }

    /// Whether `ident` contains the stem's words as a run of whole words; the last word may
    /// be plural (`programs`, `PROGRAMS`, `useProgramMembers`, `program_id`).
    pub fn matches(&self, ident: &str) -> bool {
        let w = words(ident);
        let n = self.words.len();
        if w.len() < n {
            return false;
        }
        (0..=w.len() - n).any(|i| {
            (0..n).all(|k| {
                let (have, want) = (&w[i + k], &self.words[k]);
                have == want
                    || (k == n - 1 && (*have == format!("{want}s") || *have == format!("{want}es")))
            })
        })
    }

    /// Whether `ident` is just the stem (`program`, `Programs`, `PROGRAM`): a plain word too
    /// common (comments, labels, AST node names) to count as a leftover on its own.
    pub fn is_bare(&self, ident: &str) -> bool {
        words(ident).len() == self.words.len() && self.matches(ident)
    }

    /// An extended regex git grep can use to find candidate lines: the first word, any case.
    pub fn grep_pattern(&self) -> String {
        self.first_word()
            .chars()
            .map(|c| {
                if c.is_ascii_alphabetic() {
                    format!("[{}{}]", c.to_ascii_lowercase(), c.to_ascii_uppercase())
                } else {
                    regex::escape(&c.to_string())
                }
            })
            .collect()
    }
}

/// Whether an identifier after `before` is a segment of a route or module path: it follows a
/// `/`, and the token it sits in starts like a path (`/`, `./`, `../`, `@/`, `~/`) or is a
/// URL. `Site/program` in prose is not.
fn is_path_segment(before: &str) -> bool {
    if !before.ends_with('/') {
        return false;
    }
    let token = before
        .rsplit(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | ',' | '='))
        .next()
        .unwrap_or("");
    ["/", "./", "../", "@/", "~/"]
        .iter()
        .any(|p| token.starts_with(p))
        || token.contains("://")
}

/// Identifiers in `line` that match `stem`; a route or module path segment is named with
/// its slash (`/programs`).
pub fn concept_names(stem: &Stem, line: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for (at, ident) in identifiers(line) {
        if !stem.matches(ident) {
            continue;
        }
        let name = if is_path_segment(&line[..at]) {
            format!("/{ident}")
        } else {
            ident.to_string()
        };
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_words() {
        assert_eq!(words("useProgramMembers"), ["use", "program", "members"]);
        assert_eq!(words("PROGRAM_ID"), ["program", "id"]);
        assert_eq!(words("HTMLParser2"), ["html", "parser", "2"]);
    }

    #[test]
    fn matches_whole_words() {
        let stem = Stem::new("program").unwrap();
        for yes in [
            "currentProgram",
            "programId",
            "PROGRAM_ID",
            "programs",
            "Program",
        ] {
            assert!(stem.matches(yes), "{yes}");
        }
        assert!(stem.is_bare("PROGRAMS") && !stem.is_bare("programId"));
        for no in ["programmer", "reprogram", "prog"] {
            assert!(!stem.matches(no), "{no}");
        }
        assert_eq!(
            concept_names(&stem, "nav('/programs/' + programId)"),
            ["/programs", "programId"]
        );
        assert_eq!(
            concept_names(&stem, "// Site/program and `/admin/programs`"),
            ["program", "/programs"]
        );
    }
}
