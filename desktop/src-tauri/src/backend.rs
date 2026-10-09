//! The backend, running in-process on Tauri's tokio runtime: one review server per open
//! repository (`Backend`), plus the settings-only server behind the Settings window (see
//! `settings_window.rs`), both started through `launch`.
//!
//! A listener is bound before the server task starts, so the webview can connect at once:
//! the kernel queues connections from `listen()` onward and no readiness polling is needed.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use refactor_diff_server::{App, ServerConfig, ServerHandle, bind_local};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{oneshot, watch};
use url::Url;

/// Tried first for the review UI so its origin stays the same across launches and
/// repository switches. Any free port works just as well.
pub const PREFERRED_PORT: u16 = 47821;
const STOP_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a start waits for the login shell's PATH (see `capture_shell_path`).
const PATH_TIMEOUT: Duration = Duration::from_secs(6);

/// Shown on the landing page after something went wrong.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastError {
    pub title: String,
    pub detail: String,
}

/// A server that `launch` started: its URL and the means to stop it.
pub struct Server {
    pub url: Url,
    handle: ServerHandle,
    /// Resolved by the watcher task once `serve` has returned (cleanup done).
    done: oneshot::Receiver<()>,
}

impl Server {
    /// Ask the server to stop and wait (bounded) for its cleanup to finish.
    pub async fn stop(self) {
        self.handle.shutdown();
        if tokio::time::timeout(STOP_TIMEOUT, self.done).await.is_err() {
            tracing::warn!("server ignored shutdown for {STOP_TIMEOUT:?}; abandoning it");
        }
    }
}

/// The first of `ports` that `bind` accepts (0 asks the OS for any free port).
fn choose_port<T>(ports: &[u16], mut bind: impl FnMut(u16) -> Option<T>) -> Option<T> {
    ports.iter().find_map(|&port| bind(port))
}

/// Build `config` and serve it on the first of `ports` that binds. `on_stopped` runs with
/// the error if the server ends on its own; a `Server::stop` doesn't count.
///
/// `App::build` validates the repository (with git) and may fail; the message is meant for
/// the user.
pub async fn launch(
    config: ServerConfig,
    ports: &[u16],
    on_stopped: impl FnOnce(String) + Send + 'static,
) -> Result<Server, String> {
    let server = tauri::async_runtime::spawn_blocking(move || App::build(config))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    let listener = choose_port(ports, |port| bind_local(port).ok())
        .ok_or("Couldn't find a free TCP port on 127.0.0.1.")?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let url = Url::parse(&format!("http://127.0.0.1:{port}/")).expect("valid url");
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
    let handle = server.handle();
    let (done_tx, done_rx) = oneshot::channel();

    let task = tauri::async_runtime::spawn(server.serve(listener, handle.stopped_owned()));
    // Watcher: reports a server that ends on its own (not via `Server::stop`).
    tauri::async_runtime::spawn(async move {
        let result = task.await;
        let _ = done_tx.send(());
        let detail = match result {
            Ok(Ok(())) => None, // clean shutdown
            Ok(Err(e)) => Some(e.to_string()),
            Err(e) => Some(format!("server task failed: {e}")),
        };
        if let Some(detail) = detail {
            on_stopped(detail);
        }
    });
    Ok(Server {
        url,
        handle,
        done: done_rx,
    })
}

struct Running {
    repo: PathBuf,
    generation: u64,
    server: Server,
}

/// Managed state: the review server and the repository-opening bookkeeping.
pub struct Backend {
    running: Mutex<Option<Running>>,
    generation: Mutex<u64>,
    /// PATH as the user's login shell sees it; captured once at startup (see
    /// `capture_shell_path`). `None` until known.
    path: watch::Sender<Option<String>>,
    /// Repository currently being started, if any.
    pub opening: Mutex<Option<PathBuf>>,
    /// Repository asked for while another was still starting; opened next (newest wins).
    pub pending: Mutex<Option<PathBuf>>,
    /// Something at launch (a Dock drop, a command-line argument) chose the repository, so
    /// the last one shouldn't be reopened.
    pub launch_claimed: AtomicBool,
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
            pending: Mutex::new(None),
            launch_claimed: AtomicBool::new(false),
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

    pub fn set_path(&self, path: String) {
        // Not `send`: that drops the value when nobody is subscribed yet.
        self.path.send_replace(Some(path));
    }

    /// The login shell's PATH, waiting up to `timeout` for it to be captured.
    pub async fn wait_path(&self, timeout: Duration) -> Option<String> {
        let mut rx = self.path.subscribe();
        let _ = tokio::time::timeout(timeout, rx.wait_for(|p| p.is_some())).await;
        rx.borrow().clone()
    }

    /// The login shell's PATH for a server about to start. A repository passed at launch
    /// can get here before the login shell has answered, so this waits a little.
    pub async fn path(&self) -> Option<OsString> {
        let path = self.wait_path(PATH_TIMEOUT).await;
        if path.is_none() {
            tracing::warn!("starting the backend without the login shell's PATH");
        }
        path.map(OsString::from)
    }
}

/// Capture the login shell's PATH on a worker thread (a slow ~/.zshrc must not block the
/// UI) and remember it for the backend, which needs `gh`, `node`/`tsserver` and system
/// pythons that a GUI app's own PATH doesn't include.
pub fn capture_shell_path(app: AppHandle) {
    std::thread::spawn(move || {
        let path = crate::shell_path::login_shell_path();
        tracing::info!("backend PATH: {path}");
        app.state::<Backend>().set_path(path);
    });
}

/// Capture it again (the landing page's "Check Again" after installing something).
pub async fn recapture_shell_path(app: &AppHandle) -> String {
    let path = tauri::async_runtime::spawn_blocking(crate::shell_path::login_shell_path)
        .await
        .unwrap_or_default();
    tracing::info!("backend PATH: {path}");
    app.state::<Backend>().set_path(path.clone());
    path
}

/// Start the review server for `repo`, replacing any running instance, and return the URL
/// of the review UI.
pub async fn start(app: &AppHandle, repo: &Path) -> Result<Url, String> {
    stop(app).await;
    let backend = app.state::<Backend>();
    let config = ServerConfig {
        repo: repo.to_path_buf(),
        path: backend.path().await,
        desktop: true,
        ..ServerConfig::default()
    };
    let generation = {
        let mut g = backend.generation.lock().unwrap();
        *g += 1;
        *g
    };
    tracing::info!(repo = %repo.display(), "backend starting");
    let watcher_app = app.clone();
    let server = launch(config, &[PREFERRED_PORT, 0], move |detail| {
        stopped(&watcher_app, generation, detail);
    })
    .await?;
    let url = server.url.clone();
    *backend.running.lock().unwrap() = Some(Running {
        repo: repo.to_path_buf(),
        generation,
        server,
    });
    Ok(url)
}

/// Stop the review server, if any, and wait (bounded) for its cleanup to finish.
pub async fn stop(app: &AppHandle) {
    let running = app.state::<Backend>().running.lock().unwrap().take();
    let Some(running) = running else { return };
    tracing::info!(repo = %running.repo.display(), "stopping backend");
    running.server.stop().await;
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
    crate::menu::rebuild(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_falls_back_in_order() {
        assert_eq!(choose_port(&[47821, 0], Some), Some(47821));
        assert_eq!(
            choose_port(&[47821, 0], |p| (p == 0).then_some(50000)),
            Some(50000)
        );
        assert_eq!(choose_port(&[47821, 0], |_| None::<u16>), None);
    }

    #[tokio::test]
    async fn path_wait_times_out_then_sees_the_value() {
        let backend = Backend::default();
        assert_eq!(backend.wait_path(Duration::from_millis(10)).await, None);
        backend.set_path("/bin".into());
        assert_eq!(
            backend
                .wait_path(Duration::from_millis(10))
                .await
                .as_deref(),
            Some("/bin")
        );
    }
}
