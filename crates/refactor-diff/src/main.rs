//! `refactor-diff` launcher: starts the local web UI for a repository.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use refactor_diff_server::{App, ConfigError, EDITORS, ServerConfig, bind_local, defaults_from};

const HOST: &str = "127.0.0.1";

fn editor_help() -> String {
    let names: Vec<&str> = EDITORS.iter().map(|(name, _)| *name).collect();
    format!(
        "editor for 'Open' links: one of {}, or a URL template with {{path}}, {{line}} and \
         {{col}} (default: vscode)",
        names.join(", ")
    )
}

/// Open a local web UI that collapses repetitive refactor edits in a diff.
#[derive(Debug, Parser)]
#[command(name = "refactor-diff", version, about, disable_help_subcommand = true)]
struct Cli {
    /// pre-select BASE..HEAD (or BASE...HEAD) in the UI
    #[arg(value_name = "range")]
    range: Option<String>,

    /// pre-select a GitHub pull request number
    #[arg(long, value_name = "PR")]
    pr: Option<u64>,

    /// pre-select uncommitted changes compared against BASE
    #[arg(long, value_name = "BASE")]
    worktree: Option<String>,

    /// pre-set filters: comma-separated file kinds to hide (source, tests, docs, config,
    /// other) and/or kinds of change: 'comments' (comment- and docstring-only edits),
    /// 'imports' (import-only edits), 'file-moves' (renamed files and the import updates they
    /// cause), 'moves' (functions and classes moved verbatim)
    #[arg(long, value_name = "KINDS")]
    hide: Option<String>,

    /// pre-set filters: hide files matching GLOB (repeatable), e.g. 'migrations'
    #[arg(long, value_name = "GLOB")]
    exclude: Vec<String>,

    /// Python interpreter or virtualenv used to resolve imports for code navigation
    /// (default: .venv, venv or env in the repository)
    #[arg(long, value_name = "PATH")]
    python: Option<PathBuf>,

    /// tsserver (or TypeScript's tsserver.js) used for TypeScript/JavaScript code navigation
    /// (default: node_modules/typescript in the repository, then PATH)
    #[arg(long, value_name = "PATH")]
    tsserver: Option<PathBuf>,

    #[arg(long, value_name = "NAME|TEMPLATE", default_value = "vscode", help = editor_help())]
    editor: String,

    /// git repository (default: cwd)
    #[arg(long, value_name = "REPO", default_value = ".")]
    repo: PathBuf,

    /// port to listen on (default: random)
    #[arg(long, value_name = "PORT", default_value_t = 0)]
    port: u16,

    /// don't open a browser window
    #[arg(long)]
    no_browser: bool,

    /// shut down when stdin closes (for a parent that pipes it, e.g. the desktop app)
    #[arg(long)]
    exit_with_parent: bool,

    /// serve only the settings page (/settings), with no repository
    #[arg(long)]
    settings_only: bool,

    /// the UI runs inside the desktop app
    #[arg(long, hide = true)]
    desktop: bool,
}

#[derive(Debug)]
enum Failure {
    Config(ConfigError),
    Build(refactor_diff_server::BuildError),
    Bind(u16, std::io::Error),
    Serve(std::io::Error),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Config(e) => write!(f, "{e}"),
            Failure::Build(e) => write!(f, "{e}"),
            Failure::Bind(port, e) => write!(f, "couldn't listen on {HOST}:{port}: {e}"),
            Failure::Serve(e) => write!(f, "server error: {e}"),
        }
    }
}

fn main() {
    let cli = Cli::parse();
    init_logging();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = runtime.block_on(run(cli)) {
        eprintln!("refactor-diff: {e}");
        std::process::exit(1);
    }
}

fn init_logging() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("warn,refactor_diff_server=info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
}

async fn run(cli: Cli) -> Result<(), Failure> {
    let defaults = defaults_from(
        cli.range.as_deref(),
        cli.pr,
        cli.worktree.as_deref(),
        cli.hide.as_deref(),
        &cli.exclude,
        &cli.editor,
    )
    .map_err(Failure::Config)?;
    let config = ServerConfig {
        repo: cli.repo,
        defaults,
        python: cli.python,
        tsserver: cli.tsserver,
        desktop: cli.desktop,
        settings_only: cli.settings_only,
        ..Default::default()
    };
    let app = App::build(config).map_err(Failure::Build)?;
    let what = if cli.settings_only {
        "settings".to_string()
    } else {
        app.repo().display().to_string()
    };

    let listener = bind_local(cli.port).map_err(|e| Failure::Bind(cli.port, e))?;
    let port = listener
        .local_addr()
        .map_err(|e| Failure::Bind(cli.port, e))?
        .port();
    let listener = tokio::net::TcpListener::from_std(listener).map_err(Failure::Serve)?;
    let url = format!("http://{HOST}:{port}/");
    {
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(
            stdout,
            "refactor-diff: serving {what} at {url} (Ctrl+C to stop)"
        );
        let _ = stdout.flush();
    }

    if !cli.no_browser {
        let url = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(800)).await;
            if let Err(e) = open::that_detached(&url) {
                eprintln!("refactor-diff: couldn't open a browser: {e}");
            }
        });
    }
    if cli.exit_with_parent {
        let handle = app.handle();
        std::thread::spawn(move || {
            watch_parent(std::io::stdin().lock());
            handle.shutdown();
        });
    }

    app.serve(listener, wait_for_signal())
        .await
        .map_err(Failure::Serve)
}

/// Block until `stdin` hits EOF. A parent that spawns us with a pipe on stdin closes it when
/// it exits, even when it is killed, so this is a portable way to never outlive it.
fn watch_parent(mut stdin: impl Read) {
    let mut buf = [0u8; 4096];
    loop {
        match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

/// Resolves on Ctrl+C or, on Unix, SIGTERM.
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(term) => term,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_like_argparse() {
        let cli = Cli::parse_from(["refactor-diff"]);
        assert!(!cli.exit_with_parent && !cli.no_browser);
        assert_eq!(cli.port, 0);
        assert_eq!(cli.editor, "vscode");
        assert_eq!(cli.repo, PathBuf::from("."));
        let cli = Cli::parse_from([
            "refactor-diff",
            "main..feature",
            "--exclude",
            "a",
            "--exclude",
            "b",
            "--exit-with-parent",
            "--port",
            "8765",
        ]);
        assert_eq!(cli.range.as_deref(), Some("main..feature"));
        assert_eq!(cli.exclude, vec!["a", "b"]);
        assert!(cli.exit_with_parent);
        assert_eq!(cli.port, 8765);
    }

    #[test]
    fn watch_parent_returns_on_eof() {
        watch_parent(std::io::Cursor::new(b"ignored input\n".to_vec()));
        watch_parent(std::io::empty());
    }
}
