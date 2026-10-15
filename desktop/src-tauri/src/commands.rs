//! Commands the bundled landing page calls, and the repository-opening flow shared with the
//! menu, the command line and Dock drops.

use std::path::{Path, PathBuf};

use refactor_diff_server::Defaults;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use url::Url;

use crate::{
    app_state, backend,
    backend::{Backend, LastError},
    deep_link, menu, recents, repo_match, settings_window,
};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    running: bool,
    repo: Option<String>,
    opening: Option<String>,
    last_error: Option<LastError>,
    /// An open link whose repository the landing page should ask about.
    locate: Option<Locate>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Locate {
    /// `owner/name`.
    repo: String,
    /// Where Clone puts it, as the user would write it (`~/code/name`).
    clone_target: String,
}

/// The landing page, with an optional fragment such as `#loading`.
fn landing_url(fragment: &str) -> Url {
    Url::parse(&format!("tauri://localhost/index.html{fragment}")).unwrap()
}

pub fn show_landing(app: &AppHandle, fragment: &str) {
    if let Some(win) = crate::main_window(app) {
        let _ = win.set_title("Refactor Diff");
        if let Err(e) = win.navigate(landing_url(fragment)) {
            tracing::warn!("couldn't show the landing page: {e}");
        }
    }
}

#[tauri::command]
pub async fn pick_repo(app: AppHandle) -> Result<Option<String>, String> {
    Ok(pick_folder(&app, "Open Repository").map(|p| p.to_string_lossy().into_owned()))
}

/// A blocking folder dialog that starts in the repositories folder. Off the main thread only
/// (async commands run there).
fn pick_folder(app: &AppHandle, title: &str) -> Option<PathBuf> {
    let mut dialog = app.dialog().file().set_title(title);
    let start = app_state::repos_dir();
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog
        .blocking_pick_folder()
        .and_then(|f| f.into_path().ok())
}

#[tauri::command]
pub async fn open_repo(app: AppHandle, path: String) -> Result<(), String> {
    open(app, PathBuf::from(path), Defaults::default()).await
}

/// The locate view's Choose Folder…: pick the checkout, confirm one without a remote for
/// the link's repository, then remember and open it. `false` if the user backed out.
#[tauri::command]
pub async fn locate_choose(app: AppHandle) -> Result<bool, String> {
    let slug = pending_locate(&app)?.slug;
    let Some(path) = pick_folder(&app, &format!("Where is {slug}?")) else {
        return Ok(false);
    };
    let tools = app.state::<Backend>().tools().await;
    if !repo_match::remote_slugs(&tools, &path).contains(&slug.key()) {
        let use_anyway = app
            .dialog()
            .message(format!(
                "{} has no GitHub remote for {slug}. Use it anyway?",
                path.display()
            ))
            .title("Not a checkout of this repository")
            .kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Use Anyway".into(),
                "Cancel".into(),
            ))
            .blocking_show();
        if !use_anyway {
            return Ok(false);
        }
    }
    let request = take_locate(&app)?;
    deep_link::open_found(app, &request, path).await?;
    Ok(true)
}

/// The locate view's Clone: `gh repo clone` into `<reposDir>/<name>`, then open it. An
/// existing folder there is used if it's a checkout of the repository.
#[tauri::command]
pub async fn locate_clone(app: AppHandle) -> Result<(), String> {
    let request = pending_locate(&app)?;
    let slug = &request.slug;
    let target = app_state::repos_dir().join(&slug.name);
    let tools = app.state::<Backend>().tools().await;
    if target.exists() {
        if !repo_match::remote_slugs(&tools, &target).contains(&slug.key()) {
            return Err(format!(
                "{} already exists and isn't a checkout of {slug}. Choose it with Choose Folder…, \
                 or change the repositories folder in Settings.",
                target.display()
            ));
        }
    } else {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Couldn't create {}: {e}", parent.display()))?;
        }
        let (repo, dest) = (slug.to_string(), target.clone());
        tracing::info!("cloning {repo} into {}", dest.display());
        let output = tauri::async_runtime::spawn_blocking(move || {
            tools
                .command("gh")
                .args(["repo", "clone", &repo])
                .arg(&dest)
                .output()
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("Couldn't run gh: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(if stderr.is_empty() {
                format!("gh repo clone {slug} failed ({}).", output.status)
            } else {
                stderr
            });
        }
    }
    let request = take_locate(&app)?;
    deep_link::open_found(app, &request, target).await
}

/// The locate view's Cancel: drop the link and go back to what was showing.
#[tauri::command]
pub fn locate_cancel(app: AppHandle) {
    let backend = app.state::<Backend>();
    backend.locate.lock().unwrap().take();
    match (backend.url(), crate::main_window(&app)) {
        (Some(url), Some(win)) => {
            let _ = win.navigate(url);
        }
        _ => show_landing(&app, ""),
    }
}

fn pending_locate(app: &AppHandle) -> Result<deep_link::OpenRequest, String> {
    app.state::<Backend>()
        .locate
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "There's no link waiting to be opened.".to_string())
}

fn take_locate(app: &AppHandle) -> Result<deep_link::OpenRequest, String> {
    app.state::<Backend>()
        .locate
        .lock()
        .unwrap()
        .take()
        .ok_or_else(|| "There's no link waiting to be opened.".to_string())
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
    let backend = app.state::<Backend>();
    let repo = backend.repo().map(|p| p.to_string_lossy().into_owned());
    let opening = backend
        .opening
        .lock()
        .unwrap()
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned());
    // Reported once; the page shows it until the next attempt.
    let last_error = backend.last_error.lock().unwrap().take();
    let locate = backend.locate.lock().unwrap().as_ref().map(|r| Locate {
        repo: r.slug.to_string(),
        clone_target: format!(
            "{}/{}",
            app_state::repos_dir_label().trim_end_matches('/'),
            r.slug.name
        ),
    });
    Status {
        running: repo.is_some(),
        repo,
        opening,
        last_error,
        locate,
    }
}

/// Fire-and-forget variant for callers on the main thread (menu, run-loop events).
pub fn open_in_background(app: AppHandle, repo: PathBuf) {
    tauri::async_runtime::spawn(async move {
        if let Err(e) = open(app, repo, Defaults::default()).await {
            tracing::warn!("open failed: {e}");
        }
    });
}

/// Menu: pick a folder, then open it. Non-blocking, as menu events arrive on the main thread.
pub fn pick_and_open(app: AppHandle) {
    let handle = app.clone();
    let mut dialog = app.dialog().file().set_title("Open Repository");
    let start = app_state::repos_dir();
    if start.is_dir() {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_folder(move |picked| {
        if let Some(path) = picked.and_then(|f| f.into_path().ok()) {
            open_in_background(handle, path);
        }
    });
}

/// Close the open repository: stop its backend and go back to the landing page.
pub fn close_repo(app: AppHandle) {
    app_state::store(&app).update(|s| s.last_repo = None);
    show_landing(&app, "");
    tauri::async_runtime::spawn(async move {
        backend::stop(&app).await;
        menu::rebuild(&app);
    });
}

/// Switch the window to the review UI for `repo`: show the landing page's loading state,
/// (re)start the backend, then navigate to it. On failure the landing page stays up and
/// shows what went wrong (via the command's error, `get_status`, or the event).
///
/// A request that arrives while another repository is starting replaces it: the newest one
/// is opened once the current start finishes (e.g. a Dock drop during the launch restore).
///
/// `defaults` pre-selects a source in the review UI and runs it (an open link's PR or range).
pub async fn open(app: AppHandle, repo: PathBuf, defaults: Defaults) -> Result<(), String> {
    let mut repo = repo
        .canonicalize()
        .map_err(|e| format!("{}: {e}", repo.display()))?;
    if !repo.is_dir() {
        return Err(format!("{} is not a folder.", repo.display()));
    }
    {
        let backend = app.state::<Backend>();
        let mut opening = backend.opening.lock().unwrap();
        if let Some(busy) = opening.as_ref() {
            // A repeat of the request in progress (a double click) changes nothing.
            let mut pending = backend.pending.lock().unwrap();
            let repeat = busy == &repo && defaults == Defaults::default();
            *pending = (!repeat).then_some((repo, defaults));
            return Ok(());
        }
        *opening = Some(repo.clone());
        *backend.last_error.lock().unwrap() = None;
    }
    show_landing(&app, "#loading");

    let mut defaults = defaults;
    let result = loop {
        let result = backend::start(&app, &repo, std::mem::take(&mut defaults)).await;
        let backend = app.state::<Backend>();
        let mut opening = backend.opening.lock().unwrap();
        let next = backend.pending.lock().unwrap().take();
        match next {
            Some((next, next_defaults)) => {
                *opening = Some(next.clone());
                repo = next;
                defaults = next_defaults;
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
            *app.state::<Backend>().last_error.lock().unwrap() = Some(error.clone());
            let _ = app.emit("repo-open-failed", &error);
            show_landing(&app, "");
            Err(detail)
        }
    }
}
