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
    file(app)
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(app: &AppHandle, recents: &[String]) {
    let Some(f) = file(app) else { return };
    let result = f
        .parent()
        .map(std::fs::create_dir_all)
        .unwrap_or(Ok(()))
        .and_then(|_| {
            std::fs::write(
                &f,
                serde_json::to_string_pretty(recents).unwrap_or_default(),
            )
        });
    if let Err(e) = result {
        tracing::warn!("couldn't save {}: {e}", f.display());
    }
}

/// Repositories that still exist, newest first.
pub fn list(app: &AppHandle) -> Vec<String> {
    load(app)
        .into_iter()
        .filter(|p| Path::new(p).is_dir())
        .collect()
}

pub fn push(app: &AppHandle, repo: &Path) {
    let repo = repo.to_string_lossy().into_owned();
    let mut recents = load(app);
    recents.retain(|p| p != &repo);
    recents.insert(0, repo);
    recents.truncate(MAX);
    save(app, &recents);
}

pub fn remove(app: &AppHandle, repo: &Path) {
    let repo = repo.to_string_lossy();
    let mut recents = load(app);
    recents.retain(|p| p.as_str() != repo);
    save(app, &recents);
}

pub fn clear(app: &AppHandle) {
    save(app, &[]);
}
