//! Content-derived identifiers.

use std::fmt::{Display, Write as _};

use sha1::{Digest, Sha1};

/// The first 12 hex characters of the SHA-1 of `parts` joined by `\x1f`.
///
/// Every id in a report (report, hunk, unit, group, move) and every hunk fingerprint is built
/// this way, and review-state files are keyed by those ids, so the scheme is part of the
/// on-disk contract.
pub fn short_hash<I, S>(parts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: Display,
{
    let mut joined = String::new();
    for (i, part) in parts.into_iter().enumerate() {
        if i > 0 {
            joined.push('\x1f');
        }
        write!(joined, "{part}").expect("writing to a String cannot fail");
    }
    let digest = Sha1::digest(joined.as_bytes());
    let mut hex = String::with_capacity(12);
    for byte in &digest[..6] {
        write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
    }
    hex
}

/// `short_hash` over a list of `Display` arguments of mixed types.
#[macro_export]
macro_rules! short_hash {
    ($($part:expr),+ $(,)?) => {
        $crate::short_hash([$(format!("{}", $part)),+])
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_python_sha1_prefix() {
        // hashlib.sha1("a\x1f1".encode()).hexdigest()[:12]
        assert_eq!(short_hash(["a", "1"]), "6aec555ea0de");
        assert_eq!(short_hash!("a", 1), short_hash(["a", "1"]));
        assert_eq!(short_hash(Vec::<&str>::new()), "da39a3ee5e6b");
    }
}
