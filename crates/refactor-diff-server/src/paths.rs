//! Where state lives on disk, and how it is written: the same places and the same bytes the
//! Python tool used, so existing settings and review marks keep working.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// `$XDG_CONFIG_HOME/refactor-diff`, or `~/.config/refactor-diff`.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config"));
    base.join("refactor-diff")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `json.dumps(value, indent=1, sort_keys=True)`: one-space indent, keys sorted at every
/// level, `": "` and `","` separators, non-ASCII escaped, no trailing newline.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, 0, &mut out);
    out
}

fn write_value(value: &Value, depth: usize, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&python_number(n)),
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.extend(std::iter::repeat_n(' ', depth + 1));
                write_value(item, depth + 1, out);
            }
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', depth));
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.extend(std::iter::repeat_n(' ', depth + 1));
                write_string(key, out);
                out.push_str(": ");
                write_value(&map[*key], depth + 1, out);
            }
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', depth));
            out.push('}');
        }
    }
}

fn python_number(n: &serde_json::Number) -> String {
    if let Some(f) = n.as_f64().filter(|_| !n.is_i64() && !n.is_u64()) {
        // Python repr() of a float: shortest round-trip, always with a decimal point.
        let s = format!("{f}");
        if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
            s
        } else {
            format!("{s}.0")
        }
    } else {
        n.to_string()
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c if (c as u32) < 0x7f => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// Write `text` to `path` through a `.json.tmp` sibling and a rename, optionally with a
/// permission mode (Unix only).
pub fn atomic_write(path: &Path, text: &str, mode: Option<u32>) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        if let Some(mode) = mode {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    fs::rename(&tmp, path)
}

/// `datetime.now(timezone.utc).isoformat(timespec="seconds")`.
pub fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    iso_utc(secs)
}

/// Format a Unix timestamp as `YYYY-MM-DDTHH:MM:SS+00:00`.
pub fn iso_utc(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let rem = unix_secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}+00:00")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_matches_python_dumps() {
        let v = json!({"b": [1, 2], "a": {"y": "é", "x": true, "e": {}, "l": []}, "n": null});
        let expected = "{\n \"a\": {\n  \"e\": {},\n  \"l\": [],\n  \"x\": true,\n  \"y\": \"\\u00e9\"\n },\n \"b\": [\n  1,\n  2\n ],\n \"n\": null\n}";
        assert_eq!(canonical_json(&v), expected);
        assert_eq!(canonical_json(&json!(1.5)), "1.5");
        assert_eq!(canonical_json(&json!("a\"b\\c\n")), "\"a\\\"b\\\\c\\n\"");
        assert_eq!(canonical_json(&json!("😀")), "\"\\ud83d\\ude00\"");
    }

    #[test]
    fn iso_utc_formats_like_python() {
        assert_eq!(iso_utc(1_791_460_800), "2026-10-08T12:00:00+00:00");
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00+00:00");
    }

    #[test]
    fn atomic_write_replaces_and_sets_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("settings.json");
        atomic_write(&path, "one", Some(0o600)).unwrap();
        atomic_write(&path, "two", Some(0o600)).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert!(!path.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
