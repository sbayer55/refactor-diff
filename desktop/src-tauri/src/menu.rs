//! The native menu bar. Rebuilt whenever the recents list or the open repository changes.

use std::path::{Path, PathBuf};

use tauri::{
    AppHandle, Manager, Wry,
    menu::{
        AboutMetadataBuilder, CheckMenuItemBuilder, Menu, MenuBuilder, MenuItemBuilder,
        SubmenuBuilder,
    },
};

use crate::{app_state, backend::Backend, commands, recents, settings_window};

pub fn install(app: &AppHandle) -> tauri::Result<()> {
    app.set_menu(build(app)?)?;
    app.on_menu_event(|app, event| {
        let id = event.id().as_ref();
        match id {
            "open-repo" => commands::pick_and_open(app.clone()),
            "close-repo" => commands::close_repo(app.clone()),
            "reveal-repo" => {
                if let Some(repo) = app.state::<Backend>().repo() {
                    if let Err(e) = tauri_plugin_opener::reveal_item_in_dir(repo) {
                        tracing::warn!("couldn't reveal the repository: {e}");
                    }
                }
            }
            "reopen-last" => {
                app_state::store(app).update(|s| s.reopen_last = !s.reopen_last);
                rebuild(app);
            }
            "clear-recents" => {
                recents::clear(app);
                rebuild(app);
            }
            "settings" => settings_window::open(app),
            "palette" => crate::eval_in_review(app, "openPalette()"),
            "shortcuts" | "help-shortcuts" => crate::eval_in_review(app, "showHelp()"),
            "reload" => {
                if let Some(win) = crate::main_window(app) {
                    let _ = win.reload();
                }
            }
            "zoom-reset" => zoom(app, 0.0),
            "zoom-in" => zoom(app, app_state::ZOOM_STEP),
            "zoom-out" => zoom(app, -app_state::ZOOM_STEP),
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

/// Zoom the main window's page by `delta` (0 resets) and remember it.
fn zoom(app: &AppHandle, delta: f64) {
    let state = app_state::store(app).update(|s| s.zoom = app_state::step_zoom(s.zoom, delta));
    if let Some(win) = crate::main_window(app) {
        let _ = win.set_zoom(state.zoom);
    }
}

fn build(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let info = app.package_info();
    let about = AboutMetadataBuilder::new()
        .name(Some("Refactor Diff"))
        .version(Some(info.version.to_string()))
        .copyright(Some("© 2026 Steven Bayer"))
        .credits(Some(
            "Review refactor-heavy diffs with the repeated edits collapsed.\n\
             Settings and review marks are kept in ~/.config/refactor-diff.",
        ))
        .build();
    // On macOS the first submenu is the application menu, whatever its title says.
    let app_menu = SubmenuBuilder::new(app, "Refactor Diff")
        .about(Some(about))
        .separator()
        .item(
            &MenuItemBuilder::with_id("settings", "Settings…")
                .accelerator("CmdOrCtrl+,")
                .build(app)?,
        )
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
    let home = std::env::var("HOME").ok();
    let mut open_recent = SubmenuBuilder::new(app, "Open Recent");
    for (i, path) in recent.iter().enumerate() {
        open_recent = open_recent.text(format!("recent:{i}"), label(path, home.as_deref()));
    }
    if !recent.is_empty() {
        open_recent = open_recent.separator();
    }
    open_recent = open_recent.text("clear-recents", "Clear Menu");
    let open_recent = open_recent.enabled(!recent.is_empty()).build()?;

    let has_repo = app.state::<Backend>().repo().is_some();
    let reopen_last = app_state::store(app).get().reopen_last;
    let file = SubmenuBuilder::new(app, "File")
        .item(
            &MenuItemBuilder::with_id("open-repo", "Open Repository…")
                .accelerator("CmdOrCtrl+O")
                .build(app)?,
        )
        .item(&open_recent)
        .separator()
        .item(
            &MenuItemBuilder::with_id("reveal-repo", "Reveal in Finder")
                .enabled(has_repo)
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("close-repo", "Close Repository")
                .accelerator("CmdOrCtrl+Shift+W")
                .enabled(has_repo)
                .build(app)?,
        )
        .separator()
        .item(
            &CheckMenuItemBuilder::with_id("reopen-last", "Reopen Last Repository at Launch")
                .checked(reopen_last)
                .build(app)?,
        )
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

    // The review UI's own shortcuts (⌘⇧P, ?) still work in the page; these make them
    // discoverable and reachable from the menu bar.
    let view = SubmenuBuilder::new(app, "View")
        .item(
            &MenuItemBuilder::with_id("palette", "Command Palette…")
                .accelerator("CmdOrCtrl+Shift+P")
                .enabled(has_repo)
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("shortcuts", "Keyboard Shortcuts")
                .enabled(has_repo)
                .build(app)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("reload", "Reload Page")
                .accelerator("CmdOrCtrl+R")
                .build(app)?,
        )
        .separator()
        .item(
            &MenuItemBuilder::with_id("zoom-reset", "Actual Size")
                .accelerator("CmdOrCtrl+0")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("zoom-in", "Zoom In")
                .accelerator("CmdOrCtrl+=")
                .build(app)?,
        )
        .item(
            &MenuItemBuilder::with_id("zoom-out", "Zoom Out")
                .accelerator("CmdOrCtrl+-")
                .build(app)?,
        )
        .separator()
        .fullscreen()
        .build()?;

    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .build()?;

    let help = SubmenuBuilder::new(app, "Help")
        .item(
            &MenuItemBuilder::with_id("help-shortcuts", "Keyboard Shortcuts")
                .enabled(has_repo)
                .build(app)?,
        )
        .build()?;

    MenuBuilder::new(app)
        .item(&app_menu)
        .item(&file)
        .item(&edit)
        .item(&view)
        .item(&window)
        .item(&help)
        .build()
}

/// "name — ~/parent/dir", so repositories with the same name stay distinguishable.
fn label(path: &str, home: Option<&str>) -> String {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    let parent = p
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    let parent = match home {
        Some(home)
            if !home.is_empty() && (parent == home || parent.starts_with(&format!("{home}/"))) =>
        {
            format!("~{}", &parent[home.len()..])
        }
        _ => parent,
    };
    if parent.is_empty() {
        name
    } else {
        format!("{name} — {parent}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_abbreviate_home() {
        assert_eq!(
            label("/Users/me/code/app", Some("/Users/me")),
            "app — ~/code"
        );
        assert_eq!(label("/srv/app", Some("/Users/me")), "app — /srv");
        assert_eq!(
            label("/Users/meow/app", Some("/Users/me")),
            "app — /Users/meow"
        );
        assert_eq!(label("/Users/me/app", Some("/Users/me")), "app — ~");
        assert_eq!(label("/srv/app", None), "app — /srv");
        assert_eq!(label("/", None), "/");
    }
}
