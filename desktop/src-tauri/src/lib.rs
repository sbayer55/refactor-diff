//! The desktop shell: a window that shows a bundled landing page until a repository is
//! chosen, then hosts the review UI served by the frozen Python backend (the "sidecar").
//!
//! The review UI itself uses no Tauri APIs: it talks to the sidecar over plain HTTP on
//! 127.0.0.1. It talks to the shell by navigating: "Open in editor" links are `vscode://`-style
//! URLs that this shell hands to the system, and `refactor-diff://` URLs are the app's own
//! (see `allow_navigation`). The shell calls into the page with `window.refactorDiff`.

mod app_state;
mod commands;
mod menu;
mod preflight;
mod recents;
mod settings_window;
mod sidecar;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::atomic::Ordering,
    time::Duration,
};

use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_window_state::{AppHandleExt, StateFlags};
use url::Url;

pub const WINDOW: &str = "main";

/// How long to wait at launch for a Dock drop before reopening the last repository.
const RESTORE_DELAY: Duration = Duration::from_millis(300);

pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                // Settings always opens at its own size, centered.
                .with_denylist(&[settings_window::WINDOW])
                .build(),
        )
        .manage(sidecar::SidecarManager::default())
        .invoke_handler(tauri::generate_handler![
            commands::pick_repo,
            commands::open_repo,
            commands::list_recents,
            commands::remove_recent,
            commands::get_status,
            commands::open_settings,
            preflight::preflight,
        ])
        .setup(|app| {
            app.manage(app_state::Store::load(app.handle()));
            sidecar::capture_shell_path(app.handle().clone());
            menu::install(app.handle())?;

            let handle = app.handle().clone();
            let win = WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("index.html".into()))
                .title("Refactor Diff")
                .inner_size(1280.0, 860.0)
                .min_inner_size(800.0, 500.0)
                .on_navigation(move |url| allow_navigation(&handle, url))
                .build()?;
            let zoom = app_state::store(app.handle()).get().zoom;
            if zoom != 1.0 {
                let _ = win.set_zoom(zoom);
            }

            if let Some(repo) = repo_from_args(std::env::args_os(), |p| p.is_dir()) {
                claim_launch(app.handle());
                commands::open_in_background(app.handle().clone(), repo);
            }
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            // A single-window tool: closing the main window quits (and stops the sidecars).
            (WINDOW, WindowEvent::CloseRequested { .. }) => {
                let app = window.app_handle();
                if let Err(e) = app.save_window_state(window_state_flags()) {
                    log::warn!("couldn't save the window state: {e}");
                }
                app.exit(0);
            }
            (settings_window::WINDOW, WindowEvent::Destroyed) => {
                settings_window::destroyed(window.app_handle());
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building the application")
        .run(|app, event| match event {
            RunEvent::Ready => restore_last_repo(app.clone()),
            // A folder dropped on the Dock icon or passed to `open -a "Refactor Diff"`.
            #[cfg(target_os = "macos")]
            RunEvent::Opened { urls } => {
                for url in urls {
                    match url.to_file_path() {
                        Ok(path) if path.is_dir() => {
                            claim_launch(app);
                            commands::open_in_background(app.clone(), path);
                        }
                        _ => log::warn!("ignoring opened URL {url}"),
                    }
                }
            }
            RunEvent::Exit => {
                sidecar::stop(app);
                sidecar::stop_settings(app);
            }
            _ => {}
        });
}

fn window_state_flags() -> StateFlags {
    StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED
}

/// Navigation policy for the app's windows: its own pages and the sidecars stay in the
/// webview, `refactor-diff://` is handled here, and anything else (editor links, the odd
/// https:// link) opens outside the app while the current page stays.
pub fn allow_navigation(app: &AppHandle, url: &Url) -> bool {
    match url.scheme() {
        "tauri" => true,
        "http" if url.host_str() == Some("127.0.0.1") => true,
        "refactor-diff" => {
            let app = app.clone();
            let (host, path) = (
                url.host_str().unwrap_or_default().to_string(),
                url.path().trim_matches('/').to_string(),
            );
            // Not from inside the webview's navigation callback.
            tauri::async_runtime::spawn(async move {
                match host.as_str() {
                    "settings" => settings_window::handle(&app, &path),
                    _ => log::warn!("unknown app URL refactor-diff://{host}/{path}"),
                }
            });
            false
        }
        _ => {
            log::info!("opening externally: {url}");
            if let Err(e) = tauri_plugin_opener::open_url(url.as_str(), None::<&str>) {
                log::warn!("could not open {url}: {e}");
            }
            false
        }
    }
}

/// Call `window.refactorDiff.<call>` in the review UI, if it's showing.
pub fn eval_in_review(app: &AppHandle, call: &str) {
    let Some(win) = main_window(app) else { return };
    let showing = win
        .url()
        .map(|u| u.scheme() == "http" && u.host_str() == Some("127.0.0.1"))
        .unwrap_or(false);
    if showing {
        let _ = win.eval(format!("window.refactorDiff?.{call}"));
    }
}

fn claim_launch(app: &AppHandle) {
    app.state::<sidecar::SidecarManager>()
        .launch_claimed
        .store(true, Ordering::SeqCst);
}

/// Reopen the repository from last time, unless something at launch chose one already.
fn restore_last_repo(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        std::thread::sleep(RESTORE_DELAY);
        let manager = app.state::<sidecar::SidecarManager>();
        let claimed = manager.launch_claimed.load(Ordering::SeqCst);
        let busy = manager.repo().is_some() || manager.opening.lock().unwrap().is_some();
        let state = app_state::store(&app).get();
        if let Some(repo) = app_state::restore_target(&state, claimed, busy, Path::is_dir) {
            log::info!("reopening {}", repo.display());
            commands::open_in_background(app.clone(), repo);
        }
    });
}

/// A repository passed on the command line (`open -a "Refactor Diff" --args ~/repo`).
/// Finder adds `-psn_…` arguments when launching; skip anything flag-like.
fn repo_from_args(
    args: impl IntoIterator<Item = OsString>,
    is_dir: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    args.into_iter()
        .skip(1)
        .map(PathBuf::from)
        .find(|p| !p.to_string_lossy().starts_with('-') && is_dir(p))
}

pub fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn repo_argument_skips_the_program_and_flags() {
        let is_dir = |p: &Path| p != Path::new("/not-a-dir");
        assert_eq!(
            repo_from_args(args(&["app", "-psn_0_123", "/repo"]), is_dir),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(
            repo_from_args(args(&["app", "/not-a-dir", "/repo"]), is_dir),
            Some(PathBuf::from("/repo"))
        );
        assert_eq!(repo_from_args(args(&["/repo"]), is_dir), None);
        assert_eq!(repo_from_args(args(&["app", "--flag"]), is_dir), None);
    }
}
