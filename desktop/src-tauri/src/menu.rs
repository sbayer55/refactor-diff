//! The native menu bar. Rebuilt whenever the recents list changes.

use std::path::{Path, PathBuf};

use tauri::{
    AppHandle, Wry,
    menu::{Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder},
};

use crate::{commands, recents};

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    app.set_menu(build(app)?)?;
    app.on_menu_event(|app, event| {
        let id = event.id().as_ref();
        match id {
            "open-repo" => commands::pick_and_open(app.clone()),
            "clear-recents" => {
                recents::clear(app);
                rebuild(app);
            }
            _ => {
                if let Some(index) = id
                    .strip_prefix("recent:")
                    .and_then(|i| i.parse::<usize>().ok())
                {
                    if let Some(path) = recents::list(app).get(index) {
                        commands::open_in_background(app.clone(), PathBuf::from(path));
                    }
                }
            }
        }
    });
    Ok(())
}

pub fn rebuild(app: &AppHandle) {
    match build(app) {
        Ok(menu) => {
            if let Err(e) = app.set_menu(menu) {
                tracing::warn!("couldn't update the menu: {e}");
            }
        }
        Err(e) => tracing::warn!("couldn't build the menu: {e}"),
    }
}

fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    // On macOS the first submenu is the application menu, whatever its title says.
    let app_menu = SubmenuBuilder::new(app, "Refactor Diff")
        .about(None)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    let recent = recents::list(app);
    let mut open_recent = SubmenuBuilder::new(app, "Open Recent");
    for (i, path) in recent.iter().enumerate() {
        open_recent = open_recent.text(format!("recent:{i}"), label(path));
    }
    if !recent.is_empty() {
        open_recent = open_recent.separator();
    }
    open_recent = open_recent.text("clear-recents", "Clear Menu");
    let open_recent = open_recent.enabled(!recent.is_empty()).build()?;

    let file = SubmenuBuilder::new(app, "File")
        .item(
            &MenuItemBuilder::with_id("open-repo", "Open Repository…")
                .accelerator("CmdOrCtrl+O")
                .build(app)?,
        )
        .item(&open_recent)
        .separator()
        .close_window()
        .build()?;

    // Without these, ⌘C/⌘V/⌘A don't reach the webview.
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .build()?;

    MenuBuilder::new(app)
        .item(&app_menu)
        .item(&file)
        .item(&edit)
        .item(&window)
        .build()
}

/// "name — ~/parent/dir", so repositories with the same name stay distinguishable.
fn label(path: &str) -> String {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let parent = p
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = match std::env::var("HOME") {
        Ok(home) if parent.starts_with(&home) => format!("~{}", &parent[home.len()..]),
        _ => parent,
    };
    if parent.is_empty() {
        name
    } else {
        format!("{name} — {parent}")
    }
}
