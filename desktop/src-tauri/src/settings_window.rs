//! The Settings window (⌘,): the AI assistant settings, served by a settings-only server of
//! its own so it works on the landing page and survives switching repositories. The server
//! starts the first time the window opens, stays up (closing the window keeps it, the next
//! ⌘, is instant) and stops with the app.
//!
//! Like the review UI, the page uses no Tauri APIs. It reports back by navigating to
//! `refactor-diff://settings/saved` or `…/close`, which `lib.rs` intercepts.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use refactor_diff_server::{ServerConfig, paths::home_dir, settings::expand_home};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use url::Url;

use crate::backend::{self, Backend, Server};

pub const WINDOW: &str = "settings";

/// Set while the server (and window) are starting, so a second ⌘, waits.
static OPENING: AtomicBool = AtomicBool::new(false);

struct Running {
    generation: u64,
    server: Server,
}

/// Managed state: the settings-only server, once it has been started.
#[derive(Default)]
pub struct SettingsServer {
    running: Mutex<Option<Running>>,
    generation: Mutex<u64>,
}

/// Show the Settings window, starting its server first if needed. Non-blocking.
pub fn open(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(WINDOW) {
        let _ = win.unminimize();
        let _ = win.set_focus();
        return;
    }
    if OPENING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = match server_url(&app).await {
            Ok(url) => WebviewWindowBuilder::new(&app, WINDOW, WebviewUrl::External(url))
                .title("Settings")
                .inner_size(760.0, 520.0)
                .min_inner_size(560.0, 400.0)
                .maximizable(false)
                .center()
                .on_navigation({
                    let app = app.clone();
                    move |url| crate::allow_navigation(&app, url)
                })
                .build()
                .map(|_| ())
                .map_err(|e| e.to_string()),
            Err(e) => Err(e),
        };
        OPENING.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            show_error(&app, "Couldn't open Settings", &e);
        }
    });
}

/// The settings server's URL, starting it if it isn't running.
async fn server_url(app: &AppHandle) -> Result<Url, String> {
    let settings = app.state::<SettingsServer>();
    if let Some(running) = settings.running.lock().unwrap().as_ref() {
        return Ok(running.server.url.clone());
    }
    let config = ServerConfig {
        path: app.state::<Backend>().path().await,
        desktop: true,
        settings_only: true,
        ..ServerConfig::default()
    };
    let generation = {
        let mut g = settings.generation.lock().unwrap();
        *g += 1;
        *g
    };
    tracing::info!("settings server starting");
    let watcher_app = app.clone();
    let server = backend::launch(config, &[0], move |detail| {
        stopped(&watcher_app, generation, detail);
    })
    .await?;
    let url = server.url.clone();
    *settings.running.lock().unwrap() = Some(Running { generation, server });
    Ok(url)
}

/// A `refactor-diff://settings/<what>` navigation from one of the app's pages.
pub fn handle(app: &AppHandle, what: &str, query: HashMap<String, String>) {
    match what {
        "" => open(app),
        "pick-repos-dir" => pick_repos_dir(app, query.get("start").map(String::as_str)),
        // The review UI shows the provider in its top bar; refresh it.
        "saved" => crate::eval_in_review(app, "reloadSettings()"),
        "close" => close(app),
        other => tracing::warn!("unknown settings action {other:?}"),
    }
}

/// The Repositories folder's Choose…: a folder dialog (starting at the field's value), whose
/// answer goes back into the form.
fn pick_repos_dir(app: &AppHandle, start: Option<&str>) {
    let Some(win) = app.get_webview_window(WINDOW) else {
        return;
    };
    let start = start
        .map(|s| expand_home(s, &home_dir()))
        .filter(|p| p.is_dir())
        .unwrap_or_else(home_dir);
    app.dialog()
        .file()
        .set_title("Repositories Folder")
        .set_directory(start)
        .set_parent(&win)
        .pick_folder(move |picked| {
            let Some(path) = picked.and_then(|f| f.into_path().ok()) else {
                return;
            };
            let path = abbreviate_home(&path, &home_dir());
            let arg = serde_json::Value::String(path).to_string();
            let _ = win.eval(format!("window.refactorDiffSettings?.setReposDir({arg})"));
        });
}

/// `path` with the home directory written as `~`, the way the setting shows it.
fn abbreviate_home(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn close(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(WINDOW) {
        let _ = win.close();
    }
}

/// Stop the settings server, if it was started. Blocking; for the run loop's exit event.
pub fn stop_blocking(app: &AppHandle) {
    let running = app.state::<SettingsServer>().running.lock().unwrap().take();
    let Some(running) = running else { return };
    tracing::info!("stopping settings server");
    tauri::async_runtime::block_on(running.server.stop());
}

/// The settings server ended on its own: close the window and say why. The next ⌘, starts
/// a fresh one.
fn stopped(app: &AppHandle, generation: u64, detail: String) {
    {
        let settings = app.state::<SettingsServer>();
        let mut running = settings.running.lock().unwrap();
        if running.as_ref().map(|r| r.generation) != Some(generation) {
            return; // replaced or stopped by us
        }
        *running = None;
    }
    close(app);
    show_error(app, "The settings backend stopped unexpectedly.", &detail);
}

fn show_error(app: &AppHandle, title: &str, detail: &str) {
    app.dialog()
        .message(if detail.is_empty() { title } else { detail })
        .title(title)
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_shown_as_tilde() {
        let home = Path::new("/Users/me");
        assert_eq!(abbreviate_home(Path::new("/Users/me/code"), home), "~/code");
        assert_eq!(abbreviate_home(Path::new("/Users/me"), home), "~");
        assert_eq!(abbreviate_home(Path::new("/srv/repos"), home), "/srv/repos");
    }
}
