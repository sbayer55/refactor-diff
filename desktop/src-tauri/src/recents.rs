//! Recently opened repositories: a small JSON list in the app's config directory
//! (`~/Library/Application Support/<identifier>/recents.json`), newest first.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

const MAX: usize = 10;

fn file(app: &AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|d| d.join("recents.json"))
}

fn load(app: &AppHandle) -> Vec<String> {
    file(app).map(|f| load_from(&f)).unwrap_or_default()
}

fn save(app: &AppHandle, recents: &[String]) {
    let Some(f) = file(app) else { return };
    if let Err(e) = save_to(&f, recents) {
        tracing::warn!("couldn't save {}: {e}", f.display());
    }
}

fn load_from(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_to(path: &Path, recents: &[String]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(recents).unwrap_or_default(),
    )
}

/// `recents` with `repo` moved (or added) to the front, at most `MAX` long.
fn pushed(mut recents: Vec<String>, repo: &str) -> Vec<String> {
    recents.retain(|p| p != repo);
    recents.insert(0, repo.to_string());
    recents.truncate(MAX);
    recents
}

fn removed(mut recents: Vec<String>, repo: &str) -> Vec<String> {
    recents.retain(|p| p != repo);
    recents
}

/// Repositories that still exist, newest first.
pub fn list(app: &AppHandle) -> Vec<String> {
    load(app)
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .collect()
}

pub fn push(app: &AppHandle, repo: &Path) {
    save(app, &pushed(load(app), &repo.to_string_lossy()));
}

pub fn remove(app: &AppHandle, repo: &Path) {
    save(app, &removed(load(app), &repo.to_string_lossy()));
}

pub fn clear(app: &AppHandle) {
    save(app, &[]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn push_moves_to_front_without_duplicates() {
        let recents = pushed(strings(&["/a", "/b", "/c"]), "/b");
        assert_eq!(recents, strings(&["/b", "/a", "/c"]));
        assert_eq!(pushed(vec![], "/a"), strings(&["/a"]));
    }

    #[test]
    fn push_keeps_at_most_max() {
        let many: Vec<String> = (0..MAX).map(|i| format!("/r{i}")).collect();
        let recents = pushed(many, "/new");
        assert_eq!(recents.len(), MAX);
        assert_eq!(recents[0], "/new");
        assert!(!recents.contains(&format!("/r{}", MAX - 1)));
    }

    #[test]
    fn remove_drops_only_that_repo() {
        assert_eq!(removed(strings(&["/a", "/b"]), "/a"), strings(&["/b"]));
        assert_eq!(removed(strings(&["/a"]), "/x"), strings(&["/a"]));
    }

    #[test]
    fn file_round_trip_and_corrupt_file() {
        let dir = std::env::temp_dir().join(format!("rd-recents-{}", std::process::id()));
        let path = dir.join("nested/recents.json");
        assert!(load_from(&path).is_empty());
        save_to(&path, &strings(&["/a", "/b"])).unwrap();
        assert_eq!(load_from(&path), strings(&["/a", "/b"]));
        std::fs::write(&path, "{").unwrap();
        assert!(load_from(&path).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
