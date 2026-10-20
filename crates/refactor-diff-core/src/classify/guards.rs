//! Guards: a one-sided unit that is exactly one `if` statement without an `else`, whose body
//! leaves early (`return`/`throw`/`raise` directly in the body).
//!
//! * `if (cond) { …; return; }` and `if (cond) return …;` (C-style)
//! * `if cond: return` and `if cond:` + an indented body (Python)
//!
//! Key: `guard\0-{cond}` for a deletion, `guard\0+{cond}` for an addition, `cond` being the
//! condition's token values. The signature's old (or new) side is the `if (cond)` text. A
//! deleted guard tags the unit `precondition-removed`: code now runs where it used to stop.
//! Guards are never mechanical (see `SignatureKind::never_mechanical`).

use super::{Classification, LineRange, Side, highlight, render};
use crate::lang::{Token, TokenKind};
use crate::model::{Signature, SignatureKind, UnitTag};

/// Classify a one-sided unit that adds or deletes a guard; `None` for anything else.
pub(super) fn classify_guard(
    a: &Side<'_>,
    b: &Side<'_>,
    old_range: LineRange,
    new_range: LineRange,
) -> Option<Classification> {
    let (side, rng, deleted) = match (a.len, b.len) {
        (n, 0) if n > 0 => (a, old_range, true),
        (0, n) if n > 0 => (b, new_range, false),
        _ => return None,
    };
    let toks = side.tokens();
    let sig: Vec<usize> = (0..toks.len()).filter(|&i| significant(&toks[i])).collect();
    let (&first, &last) = (sig.first()?, sig.last()?);
    if !toks[first].is(TokenKind::Name, "if") {
        return None;
    }
    let all = &side.analysis.tokens;
    let before = (0..side.offset).rev().find(|&j| significant(&all[j]));
    if before.is_some_and(|j| all[j].value == "else") {
        return None;
    }
    let after = (side.offset + side.len..all.len()).find(|&j| significant(&all[j]));
    if after.is_some_and(|j| matches!(all[j].value.as_str(), "else" | "elif")) {
        return None;
    }

    let head = c_style(toks, &sig, last).or_else(|| python_style(toks, &sig))?;
    if head.cond.is_empty() {
        return None;
    }
    let norm = head
        .cond
        .iter()
        .map(|&i| toks[i].value.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let text = render(&toks[first..head.display_end], &side.analysis.lines);
    let sign = if deleted { '-' } else { '+' };
    let key = format!("guard\0{sign}{norm}");
    let (old, new) = if deleted {
        (text, String::new())
    } else {
        (String::new(), text)
    };
    let mut cls = Classification::single(Signature::new(SignatureKind::Guard, key, old, new));
    let hl = if deleted {
        &mut cls.old_hl
    } else {
        &mut cls.new_hl
    };
    highlight(hl, toks, rng);
    if deleted {
        cls.tags.push(UnitTag::PreconditionRemoved);
    }
    Some(cls)
}

/// The `if` head, as indices into the unit's tokens.
struct Head {
    /// The condition's significant tokens, without the parentheses of a C-style `if`.
    cond: Vec<usize>,
    /// One past the last token of the `if (cond)` display text.
    display_end: usize,
}

fn significant(t: &Token) -> bool {
    !matches!(t.kind, TokenKind::Structural | TokenKind::Comment)
}

fn is_open(t: &Token) -> bool {
    t.kind != TokenKind::String && matches!(t.value.as_str(), "(" | "[" | "{" | "${")
}

fn is_close(t: &Token) -> bool {
    t.kind != TokenKind::String && matches!(t.value.as_str(), ")" | "]" | "}")
}

fn exits(t: &Token) -> bool {
    t.kind == TokenKind::Name && matches!(t.value.as_str(), "return" | "throw" | "raise")
}

/// The position in `sig` of the bracket closing the one at `sig[at]`.
fn closing(toks: &[Token], sig: &[usize], at: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (p, &i) in sig.iter().enumerate().skip(at) {
        if is_open(&toks[i]) {
            depth += 1;
        } else if is_close(&toks[i]) {
            depth -= 1;
            if depth == 0 {
                return Some(p);
            }
        }
    }
    None
}

/// `if (cond) { … return … }` or `if (cond) return …;`, ending the unit.
fn c_style(toks: &[Token], sig: &[usize], last: usize) -> Option<Head> {
    if sig.get(1).is_none_or(|&i| toks[i].value != "(") {
        return None;
    }
    let close = closing(toks, sig, 1)?;
    let body = *sig.get(close + 1)?;
    let head = Head {
        cond: sig[2..close].to_vec(),
        display_end: sig[close] + 1,
    };
    if toks[body].value == "{" {
        let end = closing(toks, sig, close + 1)?;
        if sig[end] != last {
            return None;
        }
        // An exit statement directly in the braces.
        let mut depth = 0i32;
        let exits_early = sig[close + 1..=end].iter().any(|&i| {
            let t = &toks[i];
            if is_open(t) {
                depth += 1;
            } else if is_close(t) {
                depth -= 1;
            }
            depth == 1 && exits(t)
        });
        return exits_early.then_some(head);
    }
    // A single statement: nothing may follow its end of line.
    if !exits(&toks[body]) {
        return None;
    }
    let newline = (body..toks.len()).find(|&j| toks[j].kind == TokenKind::Structural);
    newline
        .is_none_or(|n| toks[n..].iter().all(|t| !significant(t)))
        .then_some(head)
}

/// `if cond: return …` or `if cond:` with an indented body, ending the unit.
fn python_style(toks: &[Token], sig: &[usize]) -> Option<Head> {
    let mut depth = 0i32;
    let colon = sig.iter().position(|&i| {
        let t = &toks[i];
        if is_open(t) {
            depth += 1;
        } else if is_close(t) {
            depth -= 1;
        }
        depth == 0 && t.value == ":" && t.kind == TokenKind::Op
    })?;
    let head = Head {
        cond: sig[1..colon].to_vec(),
        display_end: sig[colon],
    };
    // Walk the body, tracking its indentation level and where statements start.
    let mut level = 0i32;
    let mut block = false;
    let mut line_start = true;
    let mut code = false;
    let mut found = false;
    let mut ended = false;
    let mut depth = 0i32;
    for t in &toks[sig[colon] + 1..] {
        match (t.kind, t.value.as_str()) {
            (TokenKind::Comment, _) => {}
            (TokenKind::Structural, "<INDENT>") => {
                level += 1;
                block = true;
                line_start = true;
            }
            (TokenKind::Structural, "<DEDENT>") => {
                level -= 1;
                line_start = true;
                if level <= 0 {
                    ended = true;
                }
            }
            (TokenKind::Structural, _) => {
                line_start = true;
                // A newline right after the colon opens an indented block instead.
                if code && !block && depth == 0 {
                    ended = true;
                }
            }
            _ => {
                if ended {
                    return None; // a statement after the guard
                }
                let body_level = i32::from(block);
                if depth == 0 && level == body_level && line_start && exits(t) {
                    found = true;
                }
                if is_open(t) {
                    depth += 1;
                } else if is_close(t) {
                    depth -= 1;
                }
                line_start = false;
                code = true;
            }
        }
    }
    found.then_some(head)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::*;
    use super::super::{Classification, classify};
    use crate::model::{SignatureKind, UnitTag};

    /// Classify `text` as deleted (or added) whole.
    fn one_sided(text: &str, deleted: bool) -> Classification {
        let (an, empty) = (analysis(text), analysis(""));
        if deleted {
            classify(&TestLang, &an, &empty, whole(&an), None, 0, 0)
        } else {
            classify(&TestLang, &empty, &an, None, whole(&an), 0, 0)
        }
    }

    #[test]
    fn deleted_inline_guard_is_tagged() {
        let cls = one_sided("if not user: return\n", true);
        assert_eq!(cls.signatures[0].kind, SignatureKind::Guard);
        assert_eq!(cls.signatures[0].key, "guard\0-not user");
        assert_eq!(cls.signatures[0].old, "if not user");
        assert_eq!(cls.signatures[0].label(), "delete if not user");
        assert_eq!(cls.tags, vec![UnitTag::PreconditionRemoved]);
    }

    #[test]
    fn added_c_style_guard_is_not_tagged() {
        let cls = one_sided("if (x) {\n  log(x)\n  return\n}\n", false);
        assert_eq!(cls.signatures[0].key, "guard\0+x");
        assert_eq!(cls.signatures[0].new, "if (x)");
        assert!(cls.tags.is_empty());
    }

    #[test]
    fn non_guards() {
        let kind = |text: &str| one_sided(text, true).signatures[0].kind;
        // No early exit.
        assert_ne!(kind("if (x) {\n  log(x)\n}\n"), SignatureKind::Guard);
        // The exit is nested in a callback.
        assert_ne!(
            kind("if (x) {\n  f(() => { return 1 })\n}\n"),
            SignatureKind::Guard
        );
        // More than the if statement.
        assert_ne!(kind("if not x: return\ny = 1\n"), SignatureKind::Guard);
        assert_ne!(kind("y = 1\n"), SignatureKind::Guard);
    }
}
