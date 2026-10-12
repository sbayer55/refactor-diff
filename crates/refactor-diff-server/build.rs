//! Packs the Python helper and the jedi/parso packages into one gzip tarball that the binary
//! embeds (`nav::bundle`).
//!
//! jedi and parso aren't checked in: their wheels are pinned by URL and sha256 in
//! `python/wheels.lock`, downloaded from PyPI into `OUT_DIR` on the first build and unpacked
//! straight into the archive as `vendor/`. For offline builds, set `REFACTOR_DIFF_PY_WHEELS`
//! to a directory that holds the same `.whl` files; they're checked against the same hashes.
//!
//! The archive is deterministic (sorted paths, zeroed timestamps and owners, fixed modes) so
//! the hash the binary is tagged with only changes when the Python sources do. When `python/`
//! is missing, or on docs.rs, the crate still builds, with an empty bundle and the hash `none`.

use std::env;
use std::error::Error;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

const WHEELS_ENV: &str = "REFACTOR_DIFF_PY_WHEELS";
/// Wheel licence files copied to `vendor/LICENSES/<package>-<file>`.
const LICENSE_FILES: &[&str] = &["LICENSE.txt", "AUTHORS.txt"];

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() {
    println!("cargo:rerun-if-changed=python");
    println!("cargo:rerun-if-env-changed={WHEELS_ENV}");
    println!("cargo:rerun-if-env-changed=DOCS_RS");
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let archive = out_dir.join("python-bundle.tar.gz");
    let python = Path::new(env!("CARGO_MANIFEST_DIR")).join("python");

    let (bytes, hash) = if python.is_dir() && env::var_os("DOCS_RS").is_none() {
        bundle(&python, &out_dir).unwrap_or_else(|e| panic!("build the python bundle: {e}"))
    } else {
        (Vec::new(), "none".to_string())
    };
    fs::write(&archive, &bytes).expect("write the python bundle");
    println!("cargo:rustc-env=REFACTOR_DIFF_PY_BUNDLE_HASH={hash}");
}

/// One file in the archive, at a path relative to `python/`.
struct Entry {
    path: PathBuf,
    data: Vec<u8>,
}

/// `helper.py` plus the locked wheels unpacked under `vendor/`, gzipped, and the hash of the
/// uncompressed tar (so it doesn't depend on which deflate backend flate2 was built with).
fn bundle(python: &Path, out_dir: &Path) -> Result<(Vec<u8>, String)> {
    let mut entries = Vec::new();
    let helper = python.join("helper.py");
    if helper.is_file() {
        entries.push(Entry {
            path: "helper.py".into(),
            data: fs::read(&helper)?,
        });
    }

    let lock = fs::read_to_string(python.join("wheels.lock"))?;
    let mut versions = String::new();
    for line in lock.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (url, sha256) = line
            .split_once(char::is_whitespace)
            .ok_or_else(|| format!("wheels.lock: expected `<url> <sha256>`, got {line:?}"))?;
        let wheel = fetch(url, sha256.trim(), out_dir)?;
        let name = wheel_name(url)?;
        let mut parts = name.splitn(3, '-');
        let (package, version) = (parts.next().unwrap(), parts.next().unwrap_or(""));
        versions.push_str(&format!("{package}=={version}\n"));
        unpack(&wheel, package, &mut entries).map_err(|e| format!("unpack {name}: {e}"))?;
    }
    entries.push(Entry {
        path: "vendor/VERSIONS".into(),
        data: versions.into_bytes(),
    });
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    let mut tar = tar::Builder::new(Vec::new());
    tar.mode(tar::HeaderMode::Deterministic);
    for entry in &entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(entry.data.len() as u64);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mode(if entry.path == Path::new("helper.py") {
            0o755
        } else {
            0o644
        });
        header.set_entry_type(tar::EntryType::Regular);
        tar.append_data(&mut header, &entry.path, entry.data.as_slice())?;
    }
    let tar = tar.into_inner()?;
    let hash = Sha256::digest(&tar)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&tar)?;
    Ok((gz.finish()?, hash))
}

/// The wheel's bytes, from `REFACTOR_DIFF_PY_WHEELS`, the `OUT_DIR` cache or PyPI, in that
/// order; whichever it comes from, it must match the locked hash.
fn fetch(url: &str, sha256: &str, out_dir: &Path) -> Result<Vec<u8>> {
    let name = wheel_name(url)?;
    if let Some(dir) = env::var_os(WHEELS_ENV) {
        let path = Path::new(&dir).join(name);
        let bytes = fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        check(name, &bytes, sha256)?;
        return Ok(bytes);
    }

    let cache = out_dir.join("wheels").join(name);
    if let Ok(bytes) = fs::read(&cache) {
        if check(name, &bytes, sha256).is_ok() {
            return Ok(bytes);
        }
    }
    let bytes = ureq::get(url)
        .call()
        .and_then(|mut response| {
            response
                .body_mut()
                .with_config()
                .limit(64 << 20)
                .read_to_vec()
        })
        .map_err(|e| {
            format!(
                "download {url}: {e}\n\
                 (to build offline, set {WHEELS_ENV} to a directory containing {name})"
            )
        })?;
    check(name, &bytes, sha256)?;
    fs::create_dir_all(cache.parent().unwrap())?;
    fs::write(&cache, &bytes)?;
    Ok(bytes)
}

fn check(name: &str, bytes: &[u8], sha256: &str) -> Result<()> {
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual.eq_ignore_ascii_case(sha256) {
        Ok(())
    } else {
        Err(format!("{name}: sha256 is {actual}, wheels.lock expects {sha256}").into())
    }
}

fn wheel_name(url: &str) -> Result<&str> {
    url.rsplit('/')
        .next()
        .filter(|n| n.ends_with(".whl"))
        .ok_or_else(|| format!("wheels.lock: {url} isn't a .whl URL").into())
}

/// The wheel's packages under `vendor/`, and its licence files under `vendor/LICENSES/`; the
/// rest of its `.dist-info` metadata is dropped.
fn unpack(wheel: &[u8], package: &str, entries: &mut Vec<Entry>) -> Result<()> {
    let mut zip = zip::ZipArchive::new(Cursor::new(wheel))?;
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        if file.is_dir() {
            continue;
        }
        let path = file
            .enclosed_name()
            .ok_or_else(|| format!("unsafe path {:?}", file.name()))?;
        let mut components = path.components();
        let top = components.next().unwrap().as_os_str().to_string_lossy();
        let path = if top.ends_with(".dist-info") {
            let rest = components.as_path().to_string_lossy();
            if !LICENSE_FILES.contains(&rest.as_ref()) {
                continue;
            }
            PathBuf::from(format!("vendor/LICENSES/{package}-{rest}"))
        } else if path.extension().is_some_and(|e| e == "pyc") {
            continue;
        } else {
            Path::new("vendor").join(&path)
        };
        let mut data = Vec::with_capacity(file.size() as usize);
        file.read_to_end(&mut data)?;
        entries.push(Entry { path, data });
    }
    Ok(())
}
