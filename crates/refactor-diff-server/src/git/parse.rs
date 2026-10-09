//! Parsers for git's machine-readable output. Pure functions, unit-tested on canned bytes.

use std::collections::HashMap;

use refactor_diff_core::FileStatus;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameStatus {
    pub status: FileStatus,
    pub old: Option<String>,
    pub new: String,
}

/// `git diff --name-status -M -z`: NUL-separated status/path records. Copies become
/// additions, any other status becomes a modification.
pub fn name_status_z(out: &[u8]) -> Vec<NameStatus> {
    let text = String::from_utf8_lossy(out);
    let fields: Vec<&str> = text.split('\0').collect();
    let mut entries = Vec::new();
    let mut i = 0;
    while i < fields.len() && !fields[i].is_empty() {
        let status = fields[i].as_bytes()[0];
        match status {
            b'R' | b'C' => {
                let (old, new) = (
                    fields.get(i + 1).copied().unwrap_or(""),
                    fields.get(i + 2).copied().unwrap_or(""),
                );
                if status == b'R' {
                    entries.push(NameStatus {
                        status: FileStatus::Renamed,
                        old: Some(old.to_string()),
                        new: new.to_string(),
                    });
                } else {
                    entries.push(NameStatus {
                        status: FileStatus::Added,
                        old: None,
                        new: new.to_string(),
                    });
                }
                i += 3;
            }
            _ => {
                let path = fields.get(i + 1).copied().unwrap_or("");
                let st = match status {
                    b'A' => FileStatus::Added,
                    b'D' => FileStatus::Deleted,
                    _ => FileStatus::Modified,
                };
                entries.push(NameStatus {
                    status: st,
                    old: Some(path.to_string()),
                    new: path.to_string(),
                });
                i += 2;
            }
        }
    }
    entries
}

/// `git cat-file --batch` output for `specs` (one per request line): `spec -> bytes`,
/// with missing objects left out.
pub fn cat_file_batch(out: &[u8], specs: &[String]) -> HashMap<String, Vec<u8>> {
    let mut blobs = HashMap::new();
    let mut pos = 0usize;
    for spec in specs {
        let Some(nl) = out[pos..].iter().position(|&b| b == b'\n') else {
            break;
        };
        let line = &out[pos..pos + nl];
        pos += nl + 1;
        let header: Vec<&[u8]> = line
            .split(|&b| b == b' ')
            .filter(|f| !f.is_empty())
            .collect();
        if line.ends_with(b" missing") || header.len() != 3 {
            continue;
        }
        let size: usize = std::str::from_utf8(header[2])
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let end = (pos + size).min(out.len());
        blobs.insert(spec.clone(), out[pos..end].to_vec());
        pos = end + 1;
    }
    blobs
}

/// One record of `git log --format=%x1e%H%x1f%h%x1f%s%x1f%an%x1f%aI[%x1f%b]`, split into its
/// head fields and the remaining text.
pub fn records(out: &str) -> Vec<(Vec<String>, String)> {
    out.split('\x1e')
        .filter(|r| !r.trim().is_empty())
        .map(|record| {
            let record = record.trim_matches('\n');
            let (head, rest) = record.split_once('\n').unwrap_or((record, ""));
            (
                head.split('\x1f').map(String::from).collect(),
                rest.to_string(),
            )
        })
        .collect()
}

/// `(files, insertions, deletions)` from a `--shortstat` line, zeros when absent.
pub fn shortstat(text: &str) -> (u32, u32, u32) {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r"(\d+) files? changed(?:, (\d+) insertions?\(\+\))?(?:, (\d+) deletions?\(-\))?",
        )
        .unwrap()
    });
    let Some(m) = RE.captures(text) else {
        return (0, 0, 0);
    };
    let n = |i: usize| m.get(i).and_then(|g| g.as_str().parse().ok()).unwrap_or(0);
    (n(1), n(2), n(3))
}

/// Parsed `git log --reverse --first-parent --shortstat` records.
pub fn commit_records(out: &str) -> Vec<super::Commit> {
    records(out)
        .into_iter()
        .map(|(head, rest)| {
            let f = |i: usize| head.get(i).cloned().unwrap_or_default();
            let (files, insertions, deletions) = shortstat(&rest);
            super::Commit {
                sha: f(0),
                short: f(1),
                subject: f(2),
                author: f(3),
                date: f(4),
                files,
                insertions,
                deletions,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_status() {
        let out = b"M\0a.py\0R100\0old.py\0new.py\0A\0b.py\0D\0c.py\0C50\0x\0y\0T\0t\0";
        let got = name_status_z(out);
        assert_eq!(
            got[0],
            NameStatus {
                status: FileStatus::Modified,
                old: Some("a.py".into()),
                new: "a.py".into()
            }
        );
        assert_eq!(
            got[1],
            NameStatus {
                status: FileStatus::Renamed,
                old: Some("old.py".into()),
                new: "new.py".into()
            }
        );
        assert_eq!(got[2].status, FileStatus::Added);
        assert_eq!(got[3].status, FileStatus::Deleted);
        assert_eq!(
            got[4],
            NameStatus {
                status: FileStatus::Added,
                old: None,
                new: "y".into()
            }
        );
        assert_eq!(got[5].status, FileStatus::Modified);
        assert!(name_status_z(b"").is_empty());
    }

    #[test]
    fn parses_cat_file_batch() {
        let specs = vec![
            "abc:a.py".to_string(),
            "abc:gone".to_string(),
            "abc:b.bin".to_string(),
        ];
        let mut out = b"1111 blob 5\nhello\nabc:gone missing\n2222 blob 3\n".to_vec();
        out.extend_from_slice(b"\x00\x01\x02\n");
        let blobs = cat_file_batch(&out, &specs);
        assert_eq!(blobs["abc:a.py"], b"hello");
        assert!(!blobs.contains_key("abc:gone"));
        assert_eq!(blobs["abc:b.bin"], b"\x00\x01\x02");
    }

    #[test]
    fn parses_shortstat_and_records() {
        assert_eq!(
            shortstat(" 2 files changed, 3 insertions(+), 1 deletion(-)"),
            (2, 3, 1)
        );
        assert_eq!(shortstat(" 1 file changed, 1 insertion(+)"), (1, 1, 0));
        assert_eq!(shortstat(""), (0, 0, 0));
        let out = "\x1eaaa\x1fa\x1fFirst\x1fMe\x1f2026-01-01T00:00:00+00:00\n 1 file changed, 2 insertions(+)\n\x1ebbb\x1fb\x1fSecond\x1fYou\x1f2026-01-02T00:00:00+00:00\n";
        let commits = commit_records(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "First");
        assert_eq!(commits[0].insertions, 2);
        assert_eq!(commits[1].author, "You");
        assert_eq!(commits[1].files, 0);
    }
}
