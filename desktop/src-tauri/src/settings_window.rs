//! The Settings window (⌘,): the AI assistant settings, served by a settings-only backend
//! of its own so it works on the landing page and survives switching repositories.
//!
//! Like the review UI, the page uses no Tauri APIs. It reports back by navigating to
//! `refactor-diff://settings/saved` or `…/close`, which `lib.rs` intercepts.

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

use crate::sidecar::{self, LastError};

pub const WINDOW: &str = "settings";

/// Set while the backend for a new Settings window is starting, so a second ⌘, waits.
static OPENING: AtomicBool = AtomicBool::new(false);

/// Show the Settings window, starting its backend first if needed. Non-blocking.
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
    tauri::async_runtime::spawn_blocking(move || {
        let result = sidecar::start_settings(&app).and_then(|url| {
            WebviewWindowBuilder::new(&app, WINDOW, WebviewUrl::External(url))
                .title("Settings")
                .inner_size(760.0, 440.0)
                .min_inner_size(560.0, 400.0)
                .maximizable(false)
                .center()
                .on_navigation({
                    let app = app.clone();
                    move |url| crate::allow_navigation(&app, url)
                })
                .build()
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
        OPENING.store(false, Ordering::SeqCst);
        if let Err(e) = result {
            sidecar::stop_settings(&app);
            show_error(&app, "Couldn't open Settings", &e);
        }
    });
}

/// A `refactor-diff://settings/<what>` navigation from one of the app's pages.
pub fn handle(app: &AppHandle, what: &str) {
    match what {
        "" => open(app),
        // The review UI shows the provider in its top bar; refresh it.
        "saved" => crate::eval_in_review(app, "reloadSettings()"),
        "close" => close(app),
        other => log::warn!("unknown settings action {other:?}"),
    }
}

fn close(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(WINDOW) {
        let _ = win.close();
    }
}

/// The Settings window is gone: stop its backend.
pub fn destroyed(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || sidecar::stop_settings(&app));
}

pub fn backend_crashed(app: &AppHandle, error: &LastError) {
    close(app);
    show_error(app, &error.title, &error.detail);
}

fn show_error(app: &AppHandle, title: &str, detail: &str) {
    app.dialog()
        .message(if detail.is_empty() { title } else { detail })
        .title(title)
        .kind(MessageDialogKind::Error)
        .show(|_| {});
}
