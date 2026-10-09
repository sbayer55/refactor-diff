//! The server as a value: build it from a [`ServerConfig`], serve it on a listener, and shut
//! it down. Everything the HTTP handlers share lives in [`AppState`].

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use refactor_diff_core::Report;
use tokio_util::sync::CancellationToken;

use crate::ai::{DefaultFactory, ProviderFactory};
use crate::config::ServerConfig;
use crate::exec::Tools;
use crate::git::{GhCli, Git, GitHub, SourceError};
use crate::http::error::ApiError;
use crate::nav::Navigator;
use crate::prefs::PrefsStore;
use crate::review::ReviewStore;
use crate::settings::SettingsStore;
use crate::snapshots::Snapshots;

/// Why an [`App`] couldn't be built.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// The configured path isn't inside a git repository (the path is the one configured, not
    /// a resolved one, so the message names what the user typed).
    #[error("{} is not inside a git repository", .0.display())]
    NotARepository(PathBuf, #[source] SourceError),
    #[error("couldn't create the snapshot directory: {0}")]
    Snapshots(#[source] io::Error),
}

/// What every request handler can reach.
pub struct AppState {
    /// The repository root.
    pub repo: PathBuf,
    /// `/api/config`'s `defaults`.
    pub defaults: serde_json::Value,
    pub tools: Arc<Tools>,
    pub git: Git,
    pub github: Arc<dyn GitHub>,
    /// Reports by id, for the lifetime of the server.
    pub reports: RwLock<HashMap<String, Arc<Report>>>,
    pub snapshots: Arc<Snapshots>,
    pub reviews: ReviewStore,
    pub settings: SettingsStore,
    /// Configured interpreter / tsserver for code navigation.
    pub python: Option<PathBuf>,
    pub tsserver: Option<PathBuf>,
    /// Go-to-definition / find-references over the two revisions.
    pub navigator: Navigator,
    /// Review UI preferences (`ui.json`).
    pub prefs: PrefsStore,
    /// The UI runs inside the desktop app.
    pub desktop: bool,
    /// Only the settings page and its API are served (no repository).
    pub settings_only: bool,
    /// Cancelled to stop serving.
    pub shutdown: CancellationToken,
    /// Builds the active AI provider from the settings.
    pub providers: Arc<dyn ProviderFactory>,
}

impl AppState {
    /// A cached report, or the 404 the API returns for an unknown id.
    pub fn report(&self, id: &str) -> Result<Arc<Report>, ApiError> {
        self.reports
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(ApiError::unknown_report)
    }

    pub fn insert_report(&self, report: Report) -> Arc<Report> {
        let report = Arc::new(report);
        self.reports
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(report.id.clone(), report.clone());
        report
    }

    /// Stop the long-lived helpers (language servers, provider clients) at shutdown.
    pub(crate) async fn shutdown_backends(&self) {
        self.navigator.close().await;
    }
}

/// Stops a running [`App`] from anywhere.
#[derive(Clone, Debug)]
pub struct ServerHandle {
    token: CancellationToken,
}

impl ServerHandle {
    /// Ask the server to stop; [`App::serve`] then drains connections and cleans up.
    pub fn shutdown(&self) {
        self.token.cancel();
    }

    /// Resolves once shutdown has been requested.
    pub async fn stopped(&self) {
        self.token.cancelled().await;
    }

    /// A `'static` future that resolves once the server has been told to stop.
    pub fn stopped_owned(&self) -> impl Future<Output = ()> + Send + 'static {
        self.token.clone().cancelled_owned()
    }

    pub fn is_stopped(&self) -> bool {
        self.token.is_cancelled()
    }
}

/// A built server: a router plus the state behind it.
pub struct App {
    state: Arc<AppState>,
}

impl App {
    /// Validate the repository and create the stores and the snapshot directory. With
    /// `settings_only`, the repository is not needed and nothing repository-bound is served.
    pub fn build(config: ServerConfig) -> Result<App, BuildError> {
        let tools = Arc::new(Tools::new(config.path));
        let repo = if config.settings_only {
            PathBuf::new()
        } else {
            Git::repo_root(&config.repo, &tools)
                .map_err(|e| BuildError::NotARepository(config.repo.clone(), e))?
        };
        let git = Git::new(repo.clone(), tools.clone());
        let github: Arc<dyn GitHub> = config
            .github
            .unwrap_or_else(|| Arc::new(GhCli::new(repo.clone(), tools.clone())));
        let snapshots = Arc::new(
            Snapshots::new(git.clone(), config.snapshot_dir).map_err(BuildError::Snapshots)?,
        );
        let navigator = Navigator::new(
            repo.clone(),
            Arc::clone(&snapshots),
            Arc::clone(&tools),
            config.python.clone(),
            config.tsserver.clone(),
        );
        let providers = config
            .providers
            .unwrap_or_else(|| Arc::new(DefaultFactory::new()));
        let state = AppState {
            reviews: ReviewStore::new(&repo, config.state_dir.as_deref()),
            settings: SettingsStore::new(config.state_dir.as_deref()),
            prefs: PrefsStore::new(config.state_dir.as_deref()),
            desktop: config.desktop,
            settings_only: config.settings_only,
            defaults: serde_json::to_value(&config.defaults).expect("defaults serialize"),
            repo,
            tools,
            git,
            github,
            reports: RwLock::new(HashMap::new()),
            snapshots,
            python: config.python,
            tsserver: config.tsserver,
            navigator,
            shutdown: CancellationToken::new(),
            providers,
        };
        Ok(App {
            state: Arc::new(state),
        })
    }

    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// The repository root.
    pub fn repo(&self) -> &Path {
        &self.state.repo
    }

    /// The HTTP API (every route, the SPA and its static files).
    pub fn router(&self) -> axum::Router {
        crate::http::router(self.state.clone())
    }

    pub fn handle(&self) -> ServerHandle {
        ServerHandle {
            token: self.state.shutdown.clone(),
        }
    }

    /// Serve until `shutdown` resolves or the handle is cancelled, then clean up (snapshots,
    /// language servers). Cleanup runs even when serving fails.
    pub async fn serve(
        self,
        listener: tokio::net::TcpListener,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> io::Result<()> {
        let token = self.state.shutdown.clone();
        let trigger = tokio::spawn({
            let token = token.clone();
            async move {
                shutdown.await;
                token.cancel();
            }
        });
        if let Ok(addr) = listener.local_addr() {
            tracing::info!(%addr, repo = %self.state.repo.display(), "serving");
        }
        let result = axum::serve(listener, self.router())
            .with_graceful_shutdown(token.clone().cancelled_owned())
            .await;
        trigger.abort();
        token.cancel();
        self.cleanup().await;
        result
    }

    /// Clean up without having served.
    pub async fn shutdown(self) {
        self.state.shutdown.cancel();
        self.cleanup().await;
    }

    async fn cleanup(&self) {
        self.state.shutdown_backends().await;
        let snapshots = self.state.snapshots.clone();
        if tokio::task::spawn_blocking(move || snapshots.close())
            .await
            .is_err()
        {
            tracing::warn!("snapshot cleanup panicked");
        }
    }
}

/// Bind `127.0.0.1:port` (0 picks a free port) as a non-blocking listener ready for tokio.
pub fn bind_local(port: u16) -> io::Result<std::net::TcpListener> {
    let listener = std::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Run blocking work (git, disk) off the async threads; a panic becomes a 500.
pub async fn run_blocking<T, F>(f: F) -> Result<T, ApiError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(format!("Internal error: {e}")))
}
