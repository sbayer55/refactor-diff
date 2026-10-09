//! Commands the bundled landing page calls, and the repository-opening flow shared with the
//! menu, the command line and Dock drops.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::DialogExt;
use url::Url;

use crate::{
    app_state, menu, recents, settings_window, sidecar,
    sidecar::{LastError, SidecarManager},
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    running: bool,
    repo: Option<String>,
    opening: Option<String>,
    last_error: Option<LastError>,
}

/// The landing page, with an optional fragment such as `#loading`.
fn landing_url(fragment: &str) -> Url {
    Url::parse(&format!("tauri://localhost/index.html{fragment}")).unwrap()
}

pub fn show_landing(app: &AppHandle, fragment: &str) {
    if let Some(win) = crate::main_window(app) {
        let _ = win.set_title("Refactor Diff");
        if let Err(e) = win.navigate(landing_url(fragment)) {
            log::warn!("couldn't show the landing page: {e}");
        }
    }
}

#[tauri::command]
pub async fn pick_repo(app: AppHandle) -> Result<Option<String>, String> {
    // Async commands run off the main thread, where a blocking dialog is fine.
    let picked = app
        .dialog()
        .file()
        .set_title("Open Repository")
        .blocking_pick_folder();
    Ok(picked
        .and_then(|f| f.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned()))
}

#[tauri::command]
pub async fn open_repo(app: AppHandle, path: String) -> Result<(), String> {
    open(app, PathBuf::from(path)).await
}

#[tauri::command]
pub fn list_recents(app: AppHandle) -> Vec<String> {
    recents::list(&app)
}

#[tauri::command]
pub fn remove_recent(app: AppHandle, path: String) {
    recents::remove(&app, Path::new(&path));
    menu::rebuild(&app);
}

#[tauri::command]
pub fn open_settings(app: AppHandle) {
    settings_window::open(&app);
}

#[tauri::command]
pub fn get_status(app: AppHandle) -> Status {
    let manager = app.state::<SidecarManager>();
    let repo = manager.repo().map(|p| p.to_string_lossy().into_owned());
    let opening = manager
        .opening
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());
    // Reported once; the page shows it until the next attempt.
    let last_error = manager.last_error.lock().unwrap().take();
    Status {
        running: repo.is_some(),
        repo,
        opening,
        last_error,
    }
}

/// Fire-and-forget variant for callers on the main thread (menu, run-loop events).
pub fn open_in_background(app: AppHandle, repo: PathBuf) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = open(app, repo).await {
            log::warn!("open failed: {e}");
        }
    });
}

/// Menu: pick a folder, then open it. Non-blocking, as menu events arrive on the main thread.
pub fn pick_and_open(app: AppHandle) {
    let handle = app.clone();
    app.dialog()
        .file()
        .set_title("Open Repository")
        .pick_folder(move |picked| {
            if let Some(path) = picked.and_then(|f| f.into_path().ok()) {
                open_in_background(handle, path);
            }
        });
}

/// Close the open repository: stop its backend and go back to the landing page.
pub fn close_repo(app: AppHandle) {
    app_state::store(&app).update(|s| s.last_repo = None);
    show_landing(&app, "");
    tauri::async_runtime::spawn_blocking(move || {
        sidecar::stop(&app);
        menu::rebuild(&app);
    });
}

/// Switch the window to the review UI for `repo`: show the landing page's loading state,
/// (re)start the backend, then navigate to it. On failure the landing page stays up and
/// shows what went wrong (via the command's error, `get_status`, or the event).
///
/// A request that arrives while another repository is starting replaces it: the newest one
/// is opened once the current start finishes (e.g. a Dock drop during the launch restore).
pub async fn open(app: AppHandle, repo: PathBuf) -> Result<(), String> {
    let mut repo = repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", repo.display()))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a folder.", repo.display()));
    }
    {
        let manager = app.state::<SidecarManager>();
        let mut opening = manager.opening.lock().unwrap();
        if let Some(busy) = opening.as_ref() {
            // A repeat of the request in progress (a double click) changes nothing.
            let mut pending = manager.pending.lock().unwrap();
            *pending = (busy != &repo).then_some(repo);
            return Ok(());
        }
        *opening = Some(repo.clone());
        *manager.last_error.lock().unwrap() = None;
    }
    show_landing(&app, "#loading");

    let result = loop {
        let worker = app.clone();
        let target = repo.clone();
        let result = tauri::async_runtime::spawn_blocking(move || sidecar::start(&worker, &target))
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
        let manager = app.state::<SidecarManager>();
        let mut opening = manager.opening.lock().unwrap();
        let next = manager.pending.lock().unwrap().take();
        match next {
            Some(next) => {
                *opening = Some(next.clone());
                repo = next;
            }
            None => {
                opening.take();
                break result;
            }
        }
        drop(opening);
        show_landing(&app, "#loading");
    };

    match result {
        Ok(url) => {
            recents::push(&app, &repo);
            let path = repo.to_string_lossy().into_owned();
            app_state::store(&app).update(|s| s.last_repo = Some(path));
            menu::rebuild(&app);
            if let Some(win) = crate::main_window(&app) {
                let name = repo
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let _ = win.set_title(&format!("{name} — Refactor Diff"));
                win.navigate(url).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        Err(detail) => {
            let error = LastError {
                title: format!("Couldn't open {}", repo.display()),
                detail: detail.clone(),
            };
            *app.state::<SidecarManager>().last_error.lock().unwrap() = Some(error.clone());
            let _ = app.emit("repo-open-failed", &error);
            show_landing(&app, "");
            Err(detail)
        }
    }
}
