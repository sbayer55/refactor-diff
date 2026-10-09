//! Small app-level preferences that outlive a launch: the repository to reopen and the
//! review UI's zoom. Kept in the app's config directory next to `recents.json`.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

pub const ZOOM_MIN: f64 = 0.5;
pub const ZOOM_MAX: f64 = 3.0;
pub const ZOOM_STEP: f64 = 0.1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppState {
    /// The repository that was open when the app last showed the review UI.
    pub last_repo: Option<String>,
    /// Reopen `last_repo` at launch (File ▸ Reopen Last Repository at Launch).
    pub reopen_last: bool,
    pub zoom: f64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_repo: None,
            reopen_last: true,
            zoom: 1.0,
        }
    }
}

/// Managed state: the loaded preferences and where they're saved.
pub struct Store {
    path: Option<PathBuf>,
    state: Mutex<AppState>,
}

impl Store {
    pub fn load(app: &AppHandle) -> Self {
        let path = app
            .path()
            .app_config_dir()
            .ok()
            .map(|d| d.join("state.json"));
        let state = path.as_deref().map(load_from).unwrap_or_default();
        Self {
            path,
            state: Mutex::new(state),
        }
    }

    pub fn get(&self) -> AppState {
        self.state.lock().unwrap().clone()
    }

    /// Change the state and save it.
    pub fn update(&self, f: impl FnOnce(&mut AppState)) -> AppState {
        let mut state = self.state.lock().unwrap();
        f(&mut state);
        if let Some(path) = &self.path {
            if let Err(e) = save_to(path, &state) {
                log::warn!("couldn't save {}: {e}", path.display());
            }
        }
        state.clone()
    }
}

pub fn store(app: &AppHandle) -> tauri::State<'_, Store> {
    app.state::<Store>()
}

fn load_from(path: &Path) -> AppState {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_to(path: &Path, state: &AppState) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        path,
        serde_json::to_string_pretty(state).unwrap_or_default(),
    )
}

/// Zoom after a View-menu step (`delta` of 0 resets), clamped and rounded to the step.
pub fn step_zoom(current: f64, delta: f64) -> f64 {
    if delta == 0.0 {
        return 1.0;
    }
    // Divide (not multiply by the step) so 1.1 comes out as exactly 1.1.
    let z = ((current + delta) / ZOOM_STEP).round() / (1.0 / ZOOM_STEP);
    z.clamp(ZOOM_MIN, ZOOM_MAX)
}

/// The repository to reopen at launch, if any: only when reopening is on, nothing else
/// (a Dock drop, a command-line argument, the user) already chose what to show, and the
/// folder still exists.
pub fn restore_target(
    state: &AppState,
    claimed: bool,
    busy: bool,
    is_dir: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if !state.reopen_last || claimed || busy {
        return None;
    }
    let repo = PathBuf::from(state.last_repo.as_ref()?);
    is_dir(&repo).then_some(repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_repo(repo: &str) -> AppState {
        AppState {
            last_repo: Some(repo.into()),
            ..AppState::default()
        }
    }

    #[test]
    fn restore_only_when_nothing_else_claimed_the_launch() {
        let state = with_repo("/r");
        let yes = |_: &Path| true;
        assert_eq!(
            restore_target(&state, false, false, yes),
            Some(PathBuf::from("/r"))
        );
        assert_eq!(restore_target(&state, true, false, yes), None);
        assert_eq!(restore_target(&state, false, true, yes), None);
        assert_eq!(restore_target(&state, false, false, |_| false), None);
        let off = AppState {
            reopen_last: false,
            ..state.clone()
        };
        assert_eq!(restore_target(&off, false, false, yes), None);
        assert_eq!(
            restore_target(&AppState::default(), false, false, yes),
            None
        );
    }

    #[test]
    fn missing_or_partial_file_uses_defaults() {
        let dir = std::env::temp_dir().join(format!("rd-app-state-{}", std::process::id()));
        let path = dir.join("state.json");
        assert_eq!(load_from(&path), AppState::default());

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"lastRepo": "/x"}"#).unwrap();
        assert_eq!(load_from(&path), with_repo("/x"));

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(load_from(&path), AppState::default());

        let state = AppState {
            last_repo: Some("/y".into()),
            reopen_last: false,
            zoom: 1.3,
        };
        save_to(&path, &state).unwrap();
        assert_eq!(load_from(&path), state);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn zoom_steps_are_clamped_and_rounded() {
        assert_eq!(step_zoom(1.0, ZOOM_STEP), 1.1);
        assert_eq!(step_zoom(1.0, -ZOOM_STEP), 0.9);
        assert_eq!(step_zoom(1.7, 0.0), 1.0);
        assert_eq!(step_zoom(ZOOM_MAX, ZOOM_STEP), ZOOM_MAX);
        assert_eq!(step_zoom(ZOOM_MIN, -ZOOM_STEP), ZOOM_MIN);
    }
}
