//! Packs the Python helper and the vendored jedi/parso packages into one gzip tarball that
//! the binary embeds (`nav::bundle`).
//!
//! The archive is deterministic (sorted paths, zeroed timestamps and owners, fixed modes) so
//! the hash the binary is tagged with only changes when the Python sources do. When `python/`
//! is missing the crate still builds, with an empty bundle and the hash `none`.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

fn main() {
    println!("cargo:rerun-if-changed=python");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let archive = out_dir.join("python-bundle.tar.gz");
    let python = Path::new(env!("CARGO_MANIFEST_DIR")).join("python");

    let bytes = if python.is_dir() {
        bundle(&python).expect("build the python bundle")
    } else {
        Vec::new()
    };
    fs::write(&archive, &bytes).expect("write the python bundle");

    let hash = if bytes.is_empty() {
        "none".to_string()
    } else {
        let digest = Sha256::digest(&bytes);
        digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
    };
    println!("cargo:rustc-env=REFACTOR_DIFF_PY_BUNDLE_HASH={hash}");
}

/// `helper.py` plus everything under `vendor/`, as archive paths relative to `python/`.
fn bundle(python: &Path) -> std::io::Result<Vec<u8>> {
    let mut files = Vec::new();
    let helper = python.join("helper.py");
    if helper.is_file() {
        files.push(helper);
    }
    walk(&python.join("vendor"), &mut files)?;
    files.sort();

    let gz = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(gz);
    tar.mode(tar::HeaderMode::Deterministic);
    for file in &files {
        let rel = file.strip_prefix(python).expect("inside python/");
        let data = fs::read(file)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mode(
            if rel.file_name().and_then(|n| n.to_str()) == Some("helper.py") {
                0o755
            } else {
                0o644
            },
        );
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, rel, data.as_slice())?;
    }
    let mut gz = tar.into_inner()?;
    gz.flush()?;
    gz.finish()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if name == "__pycache__" {
                continue;
            }
            walk(&path, out)?;
        } else if !name.ends_with(".pyc") {
            out.push(path);
        }
    }
    Ok(())
}
