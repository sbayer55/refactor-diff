//! Lifecycle of the frozen `refactor-diff` backend: one process per open repository.

use std::{
    collections::VecDeque,
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_shell::{
    process::{CommandChild, CommandEvent},
    ShellExt,
};
use url::Url;

/// Tried first so the review UI keeps the same origin (and so its localStorage
/// preferences) across launches and repository switches. Any free port works.
pub const PREFERRED_PORT: u16 = 47821;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
const STDERR_TAIL_LINES: usize = 40;

/// Shown on the landing page after something went wrong.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastError {
    pub title: String,
    pub detail: String,
}

struct Running {
    child: CommandChild,
    pid: u32,
    repo: PathBuf,
    /// Set before we terminate it ourselves, so the exit isn't reported as a crash.
    stopping: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct SidecarManager {
    running: Mutex<Option<Running>>,
    /// PATH as the user's login shell sees it; captured once at startup (see
    /// `capture_shell_path`). GUI apps otherwise don't see Homebrew, nvm, etc.
    shell_path: Mutex<Option<String>>,
    /// Repository currently being started, if any.
    pub opening: Mutex<Option<PathBuf>>,
    pub last_error: Mutex<Option<LastError>>,
}

impl SidecarManager {
    pub fn repo(&self) -> Option<PathBuf> {
        self.running
            .lock()
            .unwrap()
            .as_ref()
            .map(|r| r.repo.clone())
    }
}

/// Where the frozen backend lives: a bundled resource in the .app, the build output under
/// `src-tauri/sidecar/` during `tauri dev`, or whatever `REFACTOR_DIFF_SIDECAR` points at
/// (see `scripts/sidecar-dev.sh`).
fn sidecar_path(app: &AppHandle) -> PathBuf {
    if let Ok(p) = std::env::var("REFACTOR_DIFF_SIDECAR") {
        return PathBuf::from(p);
    }
    let rel = Path::new("sidecar/refactor-diff-sidecar/refactor-diff-sidecar");
    if let Ok(dir) = app.path().resource_dir() {
        let bundled = dir.join(rel);
        if bundled.exists() {
            return bundled;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn free_port() -> u16 {
    for port in [PREFERRED_PORT, 0] {
        if let Ok(listener) = TcpListener::bind(("127.0.0.1", port)) {
            if let Ok(addr) = listener.local_addr() {
                return addr.port();
            }
        }
    }
    0
}

/// Start the backend for `repo`, replacing any running instance, and return the URL of the
/// review UI once it accepts connections. Blocking; call from a worker thread.
pub fn start(app: &AppHandle, repo: &Path) -> Result<Url, String> {
    let manager = app.state::<SidecarManager>();
    stop(app);

    let program = sidecar_path(app);
    if !program.exists() {
        return Err(format!(
            "The backend is missing from this build ({}).",
            program.display()
        ));
    }
    let port = free_port();
    if port == 0 {
        return Err("Couldn't find a free TCP port on 127.0.0.1.".into());
    }

    let mut command = app
        .shell()
        .command(&program)
        .args([
            "--repo",
            &repo.to_string_lossy(),
            "--port",
            &port.to_string(),
            "--no-browser",
            "--exit-with-parent",
        ])
        .current_dir(repo);
    if let Some(path) = manager.shell_path.lock().unwrap().clone() {
        command = command.env("PATH", path);
    }
    let (mut events, child) = command
        .spawn()
        .map_err(|e| format!("Couldn't start the backend: {e}"))?;
    let pid = child.pid();
    log::info!(
        "sidecar {pid}: {} --repo {} --port {port}",
        program.display(),
        repo.display()
    );

    let stopping = Arc::new(AtomicBool::new(false));
    let exited = Arc::new(AtomicBool::new(false));
    let stderr_tail: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));

    // Drain the pipes (a full pipe would block the backend) and notice when it dies.
    let drain = {
        let app = app.clone();
        let stopping = stopping.clone();
        let exited = exited.clone();
        let stderr_tail = stderr_tail.clone();
        async move {
            while let Some(event) = events.recv().await {
                match event {
                    CommandEvent::Stdout(line) => {
                        log::info!(
                            "sidecar {pid}: {}",
                            String::from_utf8_lossy(&line).trim_end()
                        )
                    }
                    CommandEvent::Stderr(line) => {
                        let line = String::from_utf8_lossy(&line).trim_end().to_string();
                        log::warn!("sidecar {pid}: {line}");
                        let mut tail = stderr_tail.lock().unwrap();
                        if tail.len() == STDERR_TAIL_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                    CommandEvent::Error(e) => log::error!("sidecar {pid}: {e}"),
                    CommandEvent::Terminated(status) => {
                        exited.store(true, Ordering::SeqCst);
                        let how = match (status.code, status.signal) {
                            (Some(code), _) => format!("exit code {code}"),
                            (None, Some(signal)) => format!("signal {signal}"),
                            _ => "unknown reason".into(),
                        };
                        log::info!("sidecar {pid} terminated ({how})");
                        if !stopping.load(Ordering::SeqCst) {
                            crashed(
                                &app,
                                pid,
                                how,
                                stderr_tail.lock().unwrap().iter().cloned().collect(),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
    };
    tauri::async_runtime::spawn(drain);

    *manager.running.lock().unwrap() = Some(Running {
        child,
        pid,
        repo: repo.to_path_buf(),
        stopping,
        exited: exited.clone(),
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    while Instant::now() < deadline {
        if exited.load(Ordering::SeqCst) {
            // Typically "<path> is not inside a git repository" on stderr.
            let detail = stderr_tail
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            *manager.running.lock().unwrap() = None;
            return Err(if detail.is_empty() {
                "The backend exited during startup.".into()
            } else {
                detail
            });
        }
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            return Ok(Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    stop(app);
    Err(format!(
        "The backend didn't start listening within {} seconds.",
        STARTUP_TIMEOUT.as_secs()
    ))
}

/// Terminate the running backend, if any. SIGTERM first so uvicorn shuts down gracefully
/// (the app's lifespan cleanup stops tsserver and removes snapshot directories); SIGKILL if
/// it hasn't gone within the grace period. Blocking.
pub fn stop(app: &AppHandle) {
    let manager = app.state::<SidecarManager>();
    let running = manager.running.lock().unwrap().take();
    let Some(running) = running else { return };
    running.stopping.store(true, Ordering::SeqCst);
    log::info!("stopping sidecar {}", running.pid);
    // SAFETY: plain libc call with a pid we spawned ourselves.
    unsafe { libc::kill(running.pid as libc::pid_t, libc::SIGTERM) };
    let deadline = Instant::now() + SHUTDOWN_GRACE;
    while Instant::now() < deadline {
        if running.exited.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    log::warn!("sidecar {} ignored SIGTERM; killing", running.pid);
    let _ = running.child.kill();
}

/// The backend died on its own while in use: go back to the landing page with the details.
fn crashed(app: &AppHandle, pid: u32, how: String, stderr: Vec<String>) {
    let manager = app.state::<SidecarManager>();
    {
        let mut running = manager.running.lock().unwrap();
        if running.as_ref().map(|r| r.pid) != Some(pid) {
            return; // already replaced
        }
        *running = None;
    }
    let error = LastError {
        title: format!("The backend stopped unexpectedly ({how})."),
        detail: stderr.join("\n"),
    };
    *manager.last_error.lock().unwrap() = Some(error.clone());
    let _ = app.emit("sidecar-exited", &error);
    crate::commands::show_landing(app, "");
}

/// Capture the login shell's PATH on a worker thread (a slow ~/.zshrc must not block the
/// UI) and remember it for the backend, which needs `gh`, `node`/`tsserver` and system
/// pythons that a GUI app's own PATH doesn't include.
pub fn capture_shell_path(app: AppHandle) {
    std::thread::spawn(move || {
        let path = login_shell_path();
        log::info!("sidecar PATH: {path}");
        *app.state::<SidecarManager>().shell_path.lock().unwrap() = Some(path);
    });
}

fn login_shell_path() -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    // Interactive rc files may print banners, so bracket the value with markers.
    let from_shell = run_with_timeout(
        std::process::Command::new(&shell)
            .args(["-ilc", "printf '\\n<<PATH>>%s<<END>>\\n' \"$PATH\""]),
        Duration::from_secs(5),
    )
    .and_then(|out| {
        let start = out.rfind("<<PATH>>")? + "<<PATH>>".len();
        let end = out[start..].find("<<END>>")? + start;
        Some(out[start..end].trim().to_string())
    })
    .unwrap_or_default();

    let mut parts: Vec<String> = from_shell
        .split(':')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    let home = std::env::var("HOME").unwrap_or_default();
    let mut extras = vec![
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
        "/usr/sbin".into(),
        "/sbin".into(),
    ];
    if !home.is_empty() {
        extras.push(format!("{home}/.local/bin"));
        extras.push(format!("{home}/.cargo/bin"));
    }
    for extra in extras {
        if !parts.contains(&extra) {
            parts.push(extra);
        }
    }
    parts.join(":")
}

fn run_with_timeout(command: &mut std::process::Command, timeout: Duration) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut stdout, &mut buf).ok();
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return reader.join().ok(),
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                log::warn!("login shell didn't report PATH within {timeout:?}");
                let _ = child.kill();
                return None;
            }
        }
    }
}
