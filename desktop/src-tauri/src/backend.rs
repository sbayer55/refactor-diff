//! The review backend, running in-process on Tauri's tokio runtime: one server per open
//! repository.
//!
//! The listener is bound before the server task starts, so the webview can connect at once:
//! the kernel queues connections from `listen()` onward and no readiness polling is needed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use refactor_diff_server::{App, ServerConfig, ServerHandle, bind_local};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{oneshot, watch};
use url::Url;

/// Tried first so the review UI keeps the same origin (and so its localStorage preferences)
/// across launches and repository switches. Any free port works.
pub const PREFERRED_PORT: u16 = 47821;
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
/// How long the first open waits for the login-shell PATH capture.
const PATH_TIMEOUT: Duration = Duration::from_secs(6);

/// Shown on the landing page after something went wrong.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastError {
    pub title: String,
    pub detail: String,
}

struct Running {
    repo: PathBuf,
    generation: u64,
    handle: ServerHandle,
    /// Resolved by the watcher task once `serve` has returned (cleanup done).
    done: oneshot::Receiver<()>,
}

pub struct Backend {
    running: Mutex<Option<Running>>,
    generation: Mutex<u64>,
    /// Login-shell PATH, captured once at startup (see `shell_path.rs`). `None` until known.
    path: watch::Sender<Option<OsString>>,
    /// Repository currently being started, if any.
    pub opening: Mutex<Option<PathBuf>>,
    pub last_error: Mutex<Option<LastError>>,
}

impl Default for Backend {
    fn default() -> Self {
        let (path, _) = watch::channel(None);
        Self {
            running: Mutex::new(None),
            generation: Mutex::new(0),
            path,
            opening: Mutex::new(None),
            last_error: Mutex::new(None),
        }
    }
}

impl Backend {
    pub fn repo(&self) -> Option<PathBuf> {
        self.running
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.repo.clone())
    }

    pub fn set_path(&self, path: OsString) {
        let _ = self.path.send(Some(path));
    }

    /// Wait (bounded) for the PATH capture; the first open may arrive before it finishes.
    async fn path(&self) -> Option<OsString> {
        let mut rx = self.path.subscribe();
        let _ = tokio::time::timeout(PATH_TIMEOUT, rx.wait_for(|p| p.is_some())).await;
        rx.borrow().clone()
    }
}

/// Capture the login shell's PATH on a thread and hand it to the backend.
pub fn capture_shell_path(app: AppHandle) {
    std::thread::spawn(move || {
        let path = crate::shell_path::login_shell_path();
        tracing::info!("backend PATH: {path}");
        app.state::<Backend>().set_path(OsString::from(path));
    });
}

fn bind() -> Result<std::net::TcpListener, String> {
    for port in [PREFERRED_PORT, 0] {
        if let Ok(listener) = bind_local(port) {
            return Ok(listener);
        }
    }
    Err("Couldn't find a free TCP port on 127.0.0.1.".into())
}

/// Start the backend for `repo`, replacing any running instance, and return the review UI's
/// URL.
pub async fn start(app: &AppHandle, repo: &Path) -> Result<Url, String> {
    stop(app).await;
    let backend = app.state::<Backend>();
    let path = backend.path().await;

    let config = ServerConfig {
        repo: repo.to_path_buf(),
        path,
        ..ServerConfig::default()
    };
    let server = App::build(config).map_err(|e| e.to_string())?;
    let listener = bind()?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).expect("valid url");
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
    let handle = server.handle();
    let generation = {
        let mut g = backend.generation.lock().unwrap();
        *g += 1;
        *g
    };
    let (done_tx, done_rx) = oneshot::channel();

    tracing::info!(%url, repo = %repo.display(), "backend starting");
    let task = tauri::async_runtime::spawn(server.serve(listener, handle.stopped_owned()));

    // Watcher: reports a server that ends on its own (not via `stop`) to the landing page.
    let watcher_app = app.clone();
    tauri::async_runtime::spawn(async move {
        let result = task.await;
        let _ = done_tx.send(());
        let detail = match result {
            Ok(Ok(())) => None, // clean shutdown
            Ok(Err(e)) => Some(e.to_string()),
            Err(e) => Some(format!("server task failed: {e}")),
        };
        if let Some(detail) = detail {
            stopped(&watcher_app, generation, detail);
        }
    });

    *backend.running.lock().unwrap() = Some(Running {
        repo: repo.to_path_buf(),
        generation,
        handle,
        done: done_rx,
    });
    Ok(url)
}

/// Stop the running backend, if any, and wait (bounded) for its cleanup to finish.
pub async fn stop(app: &AppHandle) {
    let running = app.state::<Backend>().running.lock().unwrap().take();
    let Some(running) = running else { return };
    tracing::info!(repo = %running.repo.display(), "stopping backend");
    running.handle.shutdown();
    if tokio::time::timeout(STOP_TIMEOUT, running.done)
        .await
        .is_err()
    {
        tracing::warn!("backend ignored shutdown for {STOP_TIMEOUT:?}; abandoning it");
    }
}

/// Blocking variant for the run loop's exit event.
pub fn stop_blocking(app: &AppHandle) {
    tauri::async_runtime::block_on(stop(app));
}

/// The server ended on its own while in use: back to the landing page with the details.
fn stopped(app: &AppHandle, generation: u64, detail: String) {
    let backend = app.state::<Backend>();
    {
        let mut running = backend.running.lock().unwrap();
        if running.as_ref().map(|r| r.generation) != Some(generation) {
            return; // replaced or stopped by us
        }
        *running = None;
    }
    let error = LastError {
        title: "The backend stopped unexpectedly.".into(),
        detail,
    };
    *backend.last_error.lock().unwrap() = Some(error.clone());
    let _ = app.emit("backend-stopped", &error);
    crate::commands::show_landing(app, "");
}
