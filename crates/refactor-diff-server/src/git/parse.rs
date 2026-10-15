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

/// `git log --numstat -z --format=%x1e...` records: the `%x1f`-split head fields of each
/// commit and, when the commit touched the file, `(insertions, deletions, path)` with the
/// path the file had *after* the commit (a rename record names old then new). Binary files
/// count as zero.
pub type NumstatRecord = (Vec<String>, Option<(u32, u32, String)>);

pub fn numstat_z(out: &[u8]) -> Vec<NumstatRecord> {
    let text = String::from_utf8_lossy(out);
    text.split('\x1e')
        .filter(|r| !r.is_empty())
        .map(|record| {
            let (head, rest) = record.split_once('\0').unwrap_or((record, ""));
            let head: Vec<String> = head.split('\x1f').map(String::from).collect();
            let rest = rest.strip_prefix('\n').unwrap_or(rest);
            let mut fields = rest.split('\0');
            let Some(stat) = fields.next().filter(|s| !s.is_empty()) else {
                return (head, None);
            };
            let mut parts = stat.splitn(3, '\t');
            let n = |s: Option<&str>| s.and_then(|s| s.parse().ok()).unwrap_or(0);
            let (add, del) = (n(parts.next()), n(parts.next()));
            let path = match parts.next() {
                Some(p) if !p.is_empty() => p.to_string(),
                _ => {
                    let _old = fields.next();
                    fields.next().unwrap_or("").to_string()
                }
            };
            (head, Some((add, del, path)))
        })
        .collect()
}

/// `git blame --porcelain`: per final line, `(commit sha, line number in that commit, the
/// file's name in that commit)`.
pub fn blame_porcelain(out: &[u8]) -> Vec<(String, u32, String)> {
    let text = String::from_utf8_lossy(out);
    let mut filenames: HashMap<String, String> = HashMap::new();
    let mut lines = Vec::new();
    let mut current: Option<(String, u32)> = None;
    for line in text.split('\n') {
        if let Some(content) = line.strip_prefix('\t') {
            let _ = content;
            if let Some((sha, orig)) = current.take() {
                let name = filenames.get(&sha).cloned().unwrap_or_default();
                lines.push((sha, orig, name));
            }
            continue;
        }
        if current.is_none() {
            let mut parts = line.split(' ');
            let (Some(sha), Some(orig)) = (parts.next(), parts.next()) else {
                continue;
            };
            if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                current = Some((sha.to_string(), orig.parse().unwrap_or(0)));
            }
            continue;
        }
        if let Some(name) = line.strip_prefix("filename ") {
            if let Some((sha, _)) = &current {
                filenames.insert(sha.clone(), name.to_string());
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_numstat_z() {
        let out = b"\x1eaaa\x1fa\x1fFirst\x1fMe\x1f2026-01-01T00:00:00+00:00\0\n99\t1\tCargo.lock\0\
\x1ebbb\x1fb\x1fRename\x1fMe\x1f2026-01-02T00:00:00+00:00\0\n1652\t562\t\0old/Cargo.lock\0Cargo.lock\0\
\x1eccc\x1fc\x1fBinary\x1fMe\x1f2026-01-03T00:00:00+00:00\0\n-\t-\tx.png\0\
\x1eddd\x1fd\x1fMerge\x1fMe\x1f2026-01-04T00:00:00+00:00\0\n";
        let got = numstat_z(out);
        assert_eq!(got.len(), 4);
        assert_eq!(got[0].0[2], "First");
        assert_eq!(got[0].1, Some((99, 1, "Cargo.lock".into())));
        assert_eq!(got[1].1, Some((1652, 562, "Cargo.lock".into())));
        assert_eq!(got[2].1, Some((0, 0, "x.png".into())));
        assert_eq!(got[3].1, None);
        assert!(numstat_z(b"").is_empty());
    }

    #[test]
    fn parses_blame_porcelain() {
        let out = b"e431db1a4f51446f7ac499f9b2d18ce5c7ca155b 1 1 2\nauthor Me\nsummary First\nfilename old.py\n\tline one\n\
e431db1a4f51446f7ac499f9b2d18ce5c7ca155b 2 2\n\tline two\n\
0000000000000000000000000000000000000000 3 3 1\nauthor Not Committed Yet\nfilename new.py\n\tline three\n";
        let got = blame_porcelain(out);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[0],
            (
                "e431db1a4f51446f7ac499f9b2d18ce5c7ca155b".into(),
                1,
                "old.py".into()
            )
        );
        assert_eq!(got[1].1, 2);
        assert_eq!(got[1].2, "old.py");
        assert_eq!(got[2].0, "0".repeat(40));
        assert_eq!(got[2].2, "new.py");
        assert!(blame_porcelain(b"").is_empty());
    }

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
