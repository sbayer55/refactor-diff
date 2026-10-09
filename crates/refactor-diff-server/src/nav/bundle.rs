//! The embedded Python bundle (`helper.py` plus vendored jedi and parso), extracted once per
//! build into the user's cache directory.
//!
//! `build.rs` packs `python/` into a gzip tarball and tags it with a hash of the archive; the
//! bundle is unpacked to `<cache>/python/<hash>/` the first time Python navigation runs. A
//! `.complete` marker distinguishes a finished extraction from an interrupted one, and a
//! process extracts into a `.tmp-<pid>` sibling that it renames into place, so concurrent
//! servers never see a half-written directory.

use std::io;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tokio::sync::OnceCell;

use super::NavigationError;

static ARCHIVE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/python-bundle.tar.gz"));

/// First 16 hex digits of the archive's SHA-256; `none` when the crate was built without a
/// `python/` directory.
pub const HASH: &str = env!("REFACTOR_DIFF_PY_BUNDLE_HASH");

/// Environment variable overriding the cache directory the bundle is extracted under.
pub const CACHE_DIR_VAR: &str = "REFACTOR_DIFF_CACHE_DIR";

const COMPLETE: &str = ".complete";

/// Lazily-extracted bundle shared by one server.
#[derive(Debug, Default)]
pub struct Bundle {
    cache_dir: Option<PathBuf>,
    dir: OnceCell<PathBuf>,
}

impl Bundle {
    pub fn new(cache_dir: Option<PathBuf>) -> Self {
        Self {
            cache_dir,
            dir: OnceCell::new(),
        }
    }

    /// The extracted bundle directory (containing `helper.py` and `vendor/`).
    pub async fn dir(&self) -> Result<&Path, NavigationError> {
        if ARCHIVE.is_empty() {
            return Err(NavigationError::new(
                "Python navigation isn't available in this build: the Jedi helper wasn't bundled.",
            ));
        }
        let dir = self
            .dir
            .get_or_try_init(|| async {
                let root = self.cache_dir.clone().unwrap_or_else(cache_root);
                tokio::task::spawn_blocking(move || extract_into(&root))
                    .await
                    .map_err(|e| NavigationError(format!("Couldn't unpack the Jedi helper: {e}")))?
                    .map_err(|e| NavigationError(format!("Couldn't unpack the Jedi helper: {e}")))
            })
            .await?;
        Ok(dir.as_path())
    }
}

/// Where the bundle goes when nothing overrides it: `$REFACTOR_DIFF_CACHE_DIR`, the
/// platform cache directory, or the system temp dir.
pub fn cache_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(CACHE_DIR_VAR).filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("", "", "refactor-diff")
        .map(|dirs| dirs.cache_dir().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join("refactor-diff"))
}

/// Extract the archive under `root` (to `root/python/<hash>/`) unless that directory is
/// already complete, and return it. Blocking.
pub fn extract_into(root: &Path) -> io::Result<PathBuf> {
    extract_archive(ARCHIVE, HASH, root)
}

fn extract_archive(archive: &[u8], hash: &str, root: &Path) -> io::Result<PathBuf> {
    let versions = root.join("python");
    let dest = versions.join(hash);
    if dest.join(COMPLETE).is_file() {
        return Ok(dest);
    }
    std::fs::create_dir_all(&versions)?;

    let tmp = versions.join(format!(".tmp-{}-{}", hash, std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut tar = tar::Archive::new(GzDecoder::new(archive));
    tar.set_preserve_mtime(false);
    tar.set_preserve_permissions(true);
    tar.unpack(&tmp)?;
    std::fs::write(tmp.join(COMPLETE), hash)?;

    if dest.exists() && !dest.join(COMPLETE).is_file() {
        let _ = std::fs::remove_dir_all(&dest); // an interrupted extraction
    }
    match std::fs::rename(&tmp, &dest) {
        Ok(()) => {}
        Err(e) => {
            // Another process may have finished first; theirs is as good as ours.
            let _ = std::fs::remove_dir_all(&tmp);
            if !dest.join(COMPLETE).is_file() {
                return Err(e);
            }
        }
    }
    remove_stale(&versions, hash);
    Ok(dest)
}

/// Best-effort removal of bundles from other builds.
fn remove_stale(versions: &Path, keep: &str) {
    let Ok(entries) = std::fs::read_dir(versions) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == keep || name.starts_with(".tmp-") {
            continue;
        }
        if entry.path().is_dir() {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn archive(files: &[(&str, &str)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        for (path, text) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Regular);
            tar.append_data(&mut header, path, text.as_bytes()).unwrap();
        }
        let mut gz = tar.into_inner().unwrap();
        gz.flush().unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn extracts_once_and_removes_stale_versions() {
        let root = tempfile::tempdir().unwrap();
        let versions = root.path().join("python");
        std::fs::create_dir_all(versions.join("oldhash")).unwrap();
        std::fs::write(versions.join("oldhash").join(COMPLETE), "oldhash").unwrap();
        std::fs::create_dir_all(versions.join(".tmp-other-1")).unwrap();

        let bytes = archive(&[("helper.py", "print(1)\n"), ("vendor/jedi/__init__.py", "")]);
        let dir = extract_archive(&bytes, "abc123", root.path()).unwrap();
        assert_eq!(dir, versions.join("abc123"));
        assert_eq!(
            std::fs::read_to_string(dir.join("helper.py")).unwrap(),
            "print(1)\n"
        );
        assert!(dir.join("vendor/jedi/__init__.py").is_file());
        assert_eq!(
            std::fs::read_to_string(dir.join(COMPLETE)).unwrap(),
            "abc123"
        );
        assert!(!versions.join("oldhash").exists(), "stale version removed");
        assert!(
            versions.join(".tmp-other-1").exists(),
            "in-progress siblings kept"
        );

        // A complete directory is reused, even when its contents differ from the archive.
        std::fs::write(dir.join("helper.py"), "changed").unwrap();
        let again = extract_archive(&bytes, "abc123", root.path()).unwrap();
        assert_eq!(again, dir);
        assert_eq!(
            std::fs::read_to_string(dir.join("helper.py")).unwrap(),
            "changed"
        );
    }

    #[test]
    fn incomplete_directory_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let dest = root.path().join("python").join("h");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("junk"), "").unwrap();
        let bytes = archive(&[("helper.py", "ok")]);
        // No marker: the half-written directory is replaced, never reused.
        let dir = extract_archive(&bytes, "h", root.path()).unwrap();
        assert_eq!(dir, dest);
        assert!(dest.join(COMPLETE).is_file());
        assert!(!dest.join("junk").exists());
        assert_eq!(
            std::fs::read_to_string(dest.join("helper.py")).unwrap(),
            "ok"
        );
    }

    #[test]
    fn real_bundle_has_the_helper_and_vendor_packages() {
        if ARCHIVE.is_empty() {
            eprintln!("note: built without python/; skipping");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let dir = extract_into(root.path()).unwrap();
        assert_eq!(dir, root.path().join("python").join(HASH));
        assert!(dir.join("helper.py").is_file());
        assert!(dir.join("vendor/jedi/__init__.py").is_file());
        assert!(dir.join("vendor/parso/__init__.py").is_file());
        assert!(
            dir.join("vendor/jedi/third_party/typeshed/stdlib/builtins.pyi")
                .is_file()
        );
        assert!(dir.join("vendor/VERSIONS").is_file());
        assert_eq!(HASH.len(), 16);
    }
}
