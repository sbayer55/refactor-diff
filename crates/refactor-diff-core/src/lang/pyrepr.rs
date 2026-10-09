//! Python string-literal evaluation and CPython `repr()` rendering.
//!
//! String token values are compared by their evaluated contents so `'x'`, `"x"` and `"""x"""`
//! classify as the same token. The value is rendered the way CPython's `repr()` would, because
//! `replace` signature keys embed token values and group ids hash those keys; existing review
//! marks depend on the exact bytes.

use unicode_general_category::{GeneralCategory, get_general_category};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PyLiteral {
    Str(String),
    Bytes(Vec<u8>),
}

/// Evaluate one Python string or bytes literal (prefixes `r`, `b`, `u` in any case and order;
/// single, double or triple quotes). Returns `None` for f-strings, malformed literals and
/// escapes this evaluator cannot resolve (`\N{...}`, lone surrogates).
pub fn eval_python_string(text: &str) -> Option<PyLiteral> {
    let mut raw = false;
    let mut bytes = false;
    let mut i = 0;
    let chars: Vec<char> = text.chars().collect();
    while i < chars.len() && i < 2 {
        match chars[i].to_ascii_lowercase() {
            'r' => raw = true,
            'b' => bytes = true,
            'u' => {}
            'f' | 't' => return None,
            _ => break,
        }
        i += 1;
    }
    let rest = &chars[i..];
    let quote_len = if rest.len() >= 6
        && (rest.starts_with(&['\'', '\'', '\'']) || rest.starts_with(&['"', '"', '"']))
    {
        3
    } else if rest.len() >= 2 && (rest[0] == '\'' || rest[0] == '"') {
        1
    } else {
        return None;
    };
    let quote = rest[0];
    let end = rest.len().checked_sub(quote_len)?;
    if end < quote_len || !rest[end..].iter().all(|&c| c == quote) {
        return None;
    }
    let body = &rest[quote_len..end];
    let decoded: Vec<char> = if raw {
        body.to_vec()
    } else {
        unescape(body, bytes)?
    };
    if bytes {
        let mut out = Vec::with_capacity(decoded.len());
        for c in decoded {
            let code = c as u32;
            if code > 0xff {
                return None;
            }
            out.push(code as u8);
        }
        Some(PyLiteral::Bytes(out))
    } else {
        Some(PyLiteral::Str(decoded.into_iter().collect()))
    }
}

fn unescape(body: &[char], bytes: bool) -> Option<Vec<char>> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        let c = body[i];
        if c != '\\' {
            if bytes && (c as u32) > 0x7f {
                return None;
            }
            out.push(c);
            i += 1;
            continue;
        }
        let Some(&e) = body.get(i + 1) else {
            out.push('\\');
            break;
        };
        i += 2;
        match e {
            '\n' => {}
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'a' => out.push('\u{7}'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\u{b}'),
            '0'..='7' => {
                let mut value = e.to_digit(8).unwrap();
                let mut n = 1;
                while n < 3 && i < body.len() && body[i].is_digit(8) {
                    value = value * 8 + body[i].to_digit(8).unwrap();
                    i += 1;
                    n += 1;
                }
                out.push(char::from_u32(value)?);
            }
            'x' => {
                let value = hex(body, &mut i, 2)?;
                out.push(char::from_u32(value)?);
            }
            'u' if !bytes => {
                let value = hex(body, &mut i, 4)?;
                out.push(char::from_u32(value)?);
            }
            'U' if !bytes => {
                let value = hex(body, &mut i, 8)?;
                out.push(char::from_u32(value)?);
            }
            'N' if !bytes => return None,
            other => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    Some(out)
}

fn hex(body: &[char], i: &mut usize, digits: usize) -> Option<u32> {
    let end = *i + digits;
    if end > body.len() {
        return None;
    }
    let mut value = 0u32;
    for &c in &body[*i..end] {
        value = value * 16 + c.to_digit(16)?;
    }
    *i = end;
    Some(value)
}

/// CPython `repr()` of a str or bytes value.
pub fn py_repr(lit: &PyLiteral) -> String {
    match lit {
        PyLiteral::Str(s) => py_repr_str(s),
        PyLiteral::Bytes(b) => py_repr_bytes(b),
    }
}

/// CPython `repr()` of a str.
pub fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        let code = ch as u32;
        if ch == quote || ch == '\\' {
            out.push('\\');
            out.push(ch);
        } else if ch == '\t' {
            out.push_str("\\t");
        } else if ch == '\n' {
            out.push_str("\\n");
        } else if ch == '\r' {
            out.push_str("\\r");
        } else if code < 0x20 || code == 0x7f {
            out.push_str(&format!("\\x{code:02x}"));
        } else if code < 0x7f || is_printable(ch) {
            out.push(ch);
        } else if code <= 0xff {
            out.push_str(&format!("\\x{code:02x}"));
        } else if code <= 0xffff {
            out.push_str(&format!("\\u{code:04x}"));
        } else {
            out.push_str(&format!("\\U{code:08x}"));
        }
    }
    out.push(quote);
    out
}

/// CPython `repr()` of a bytes value.
pub fn py_repr_bytes(b: &[u8]) -> String {
    let quote = if b.contains(&b'\'') && !b.contains(&b'"') {
        b'"'
    } else {
        b'\''
    };
    let mut out = String::with_capacity(b.len() + 3);
    out.push('b');
    out.push(quote as char);
    for &byte in b {
        if byte == quote || byte == b'\\' {
            out.push('\\');
            out.push(byte as char);
        } else if byte == b'\t' {
            out.push_str("\\t");
        } else if byte == b'\n' {
            out.push_str("\\n");
        } else if byte == b'\r' {
            out.push_str("\\r");
        } else if !(0x20..0x7f).contains(&byte) {
            out.push_str(&format!("\\x{byte:02x}"));
        } else {
            out.push(byte as char);
        }
    }
    out.push(quote as char);
    out
}

/// `str.isprintable()` for one character.
fn is_printable(ch: char) -> bool {
    if ch == ' ' {
        return true;
    }
    !matches!(
        get_general_category(ch),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::SpaceSeparator
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(text: &str) -> String {
        py_repr(&eval_python_string(text).expect("evaluates"))
    }

    #[test]
    fn quote_styles_normalize() {
        assert_eq!(s("'x'"), "'x'");
        assert_eq!(s("\"x\""), "'x'");
        assert_eq!(s("\"\"\"x\"\"\""), "'x'");
        assert_eq!(s("'''it's'''"), "\"it's\"");
        assert_eq!(s("'a\\'b\"c'"), "'a\\'b\"c'");
        assert_eq!(s("u'x'"), "'x'");
        assert_eq!(s("R'a\\nb'"), "'a\\\\nb'");
        assert_eq!(s("'a\\nb'"), "'a\\nb'");
        assert_eq!(s("'\\x00\\t'"), "'\\x00\\t'");
        assert_eq!(s("'\\u00e9'"), "'é'");
        assert_eq!(s("'\\u200b'"), "'\\u200b'");
        assert_eq!(s("'\\U0001f600'"), "'😀'");
        assert_eq!(s("'\\q'"), "'\\\\q'");
        assert_eq!(s("'\\101'"), "'A'");
        assert_eq!(s("b'ab\\x01'"), "b'ab\\x01'");
        assert_eq!(s("b\"it's\""), "b\"it's\"");
        assert_eq!(s("'a\\\nb'"), "'ab'");
    }

    #[test]
    fn unsupported_literals_return_none() {
        assert!(eval_python_string("f'x'").is_none());
        assert!(eval_python_string("'\\N{BULLET}'").is_none());
        assert!(eval_python_string("b'é'").is_none());
        assert!(eval_python_string("'unterminated").is_none());
        assert!(eval_python_string("'\\ud83d'").is_none());
    }
}
