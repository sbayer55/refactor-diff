//! The desktop shell: a window that shows a bundled landing page until a repository is
//! chosen, then hosts the review UI served by the frozen Python backend (the "sidecar").
//!
//! The review UI itself is unchanged and uses no Tauri APIs: it talks to the sidecar over
//! plain HTTP on 127.0.0.1, and its "Open in editor" links are ordinary navigations to
//! `vscode://`-style URLs that this shell intercepts and hands to the system.

mod commands;
mod menu;
mod recents;
mod sidecar;

use std::path::PathBuf;

use tauri::{AppHandle, Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub const WINDOW: &str = "main";

pub fn run() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(sidecar::SidecarManager::default())
        .invoke_handler(tauri::generate_handler![
            commands::pick_repo,
            commands::open_repo,
            commands::list_recents,
            commands::remove_recent,
            commands::get_status,
        ])
        .setup(|app| {
            sidecar::capture_shell_path(app.handle().clone());
            menu::install(app.handle())?;

            WebviewWindowBuilder::new(app, WINDOW, WebviewUrl::App("index.html".into()))
                .title("Refactor Diff")
                .inner_size(1280.0, 860.0)
                .min_inner_size(800.0, 500.0)
                .on_navigation(|url| {
                    let internal = url.scheme() == "tauri"
                        || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
                    if !internal {
                        // Editor links (vscode://, cursor://, …) and the odd https:// link:
                        // open them outside the app and keep the current page.
                        log::info!("opening externally: {url}");
                        if let Err(e) = tauri_plugin_opener::open_url(url.as_str(), None::<&str>) {
                            log::warn!("could not open {url}: {e}");
                        }
                    }
                    internal
                })
                .build()?;

            if let Some(repo) = repo_from_args() {
                commands::open_in_background(app.handle().clone(), repo);
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // A single-window tool: closing the window quits (and stops the sidecar).
            if let WindowEvent::CloseRequested { .. } = event {
                window.app_handle().exit(0);
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building the application")
        .run(|app, event| match event {
            // A folder dropped on the Dock icon or passed to `open -a "Refactor Diff"`.
            #[cfg(target_os = "macos")]
            RunEvent::Opened { urls } => {
                for url in urls {
                    match url.to_file_path() {
                        Ok(path) if path.is_dir() => {
                            commands::open_in_background(app.clone(), path);
                        }
                        _ => log::warn!("ignoring opened URL {url}"),
                    }
                }
            }
            RunEvent::Exit => sidecar::stop(app),
            _ => {}
        });
}

/// A repository passed on the command line (`open -a "Refactor Diff" --args ~/repo`).
/// Finder adds `-psn_…` arguments when launching; skip anything flag-like.
fn repo_from_args() -> Option<PathBuf> {
    std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .find(|p| !p.to_string_lossy().starts_with('-') && p.is_dir())
}

pub fn main_window(app: &AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window(WINDOW)
}
