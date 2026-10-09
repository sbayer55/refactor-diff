//! Lifecycle of the frozen `refactor-diff` backend: one process per open repository, plus a
//! small settings-only one while the Settings window is open.

use std::{
    collections::VecDeque,
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
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

/// Tried first for the review UI so its origin stays the same across launches. The UI keeps
/// its preferences on the server now, so any free port works just as well.
pub const PREFERRED_PORT: u16 = 47821;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);
/// How long a start waits for the login shell's PATH (see `capture_shell_path`).
const SHELL_PATH_WAIT: Duration = Duration::from_secs(6);
const STDERR_TAIL_LINES: usize = 40;

/// Shown on the landing page after something went wrong.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LastError {
    pub title: String,
    pub detail: String,
}

/// Which backend: the one serving the review UI, or the one behind the Settings window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slot {
    Review,
    Settings,
}

struct Running {
    child: CommandChild,
    pid: u32,
    repo: Option<PathBuf>,
    /// Set before we terminate it ourselves, so the exit isn't reported as a crash.
    stopping: Arc<AtomicBool>,
    exited: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct SidecarManager {
    running: Mutex<Option<Running>>,
    settings: Mutex<Option<Running>>,
    /// PATH as the user's login shell sees it; captured once at startup (see
    /// `capture_shell_path`). GUI apps otherwise don't see Homebrew, nvm, etc.
    shell_path: Mutex<Option<String>>,
    shell_path_ready: Condvar,
    /// Repository currently being started, if any.
    pub opening: Mutex<Option<PathBuf>>,
    /// Repository asked for while another was still starting; opened next (newest wins).
    pub pending: Mutex<Option<PathBuf>>,
    /// Something at launch (a Dock drop, a command-line argument) chose the repository, so
    /// the last one shouldn't be reopened.
    pub launch_claimed: AtomicBool,
    pub last_error: Mutex<Option<LastError>>,
}

impl SidecarManager {
    pub fn repo(&self) -> Option<PathBuf> {
        self.running
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|r| r.repo.clone())
    }

    fn slot(&self, slot: Slot) -> &Mutex<Option<Running>> {
        match slot {
            Slot::Review => &self.running,
            Slot::Settings => &self.settings,
        }
    }

    /// The login shell's PATH, waiting up to `timeout` for it to be captured.
    pub fn wait_shell_path(&self, timeout: Duration) -> Option<String> {
        let guard = self.shell_path.lock().unwrap();
        let (guard, _) = self
            .shell_path_ready
            .wait_timeout_while(guard, timeout, |p| p.is_none())
            .unwrap();
        guard.clone()
    }

    fn set_shell_path(&self, path: String) {
        *self.shell_path.lock().unwrap() = Some(path);
        self.shell_path_ready.notify_all();
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

/// The first of `candidates` that `bind` accepts (0 asks the OS for any free port).
fn choose_port(candidates: &[u16], mut bind: impl FnMut(u16) -> Option<u16>) -> Option<u16> {
    candidates.iter().find_map(|&port| bind(port))
}

fn bind_local(port: u16) -> Option<u16> {
    let listener = TcpListener::bind(("127.0.0.1", port)).ok()?;
    listener.local_addr().ok().map(|addr| addr.port())
}

/// Start the backend for `repo`, replacing any running instance, and return the URL of the
/// review UI once it accepts connections. Blocking; call from a worker thread.
pub fn start(app: &AppHandle, repo: &Path) -> Result<Url, String> {
    stop(app);
    let port = choose_port(&[PREFERRED_PORT, 0], bind_local)
        .ok_or("Couldn't find a free TCP port on 127.0.0.1.")?;
    let repo_arg = repo.to_string_lossy().into_owned();
    spawn(
        app,
        Slot::Review,
        port,
        &["--repo", &repo_arg],
        repo,
        Some(repo.to_path_buf()),
    )
}

/// Start the settings-only backend for the Settings window and return its URL. Blocking.
pub fn start_settings(app: &AppHandle) -> Result<Url, String> {
    stop_settings(app);
    let port =
        choose_port(&[0], bind_local).ok_or("Couldn't find a free TCP port on 127.0.0.1.")?;
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| "/".into());
    spawn(app, Slot::Settings, port, &["--settings-only"], &home, None)
}

fn spawn(
    app: &AppHandle,
    slot: Slot,
    port: u16,
    args: &[&str],
    cwd: &Path,
    repo: Option<PathBuf>,
) -> Result<Url, String> {
    let manager = app.state::<SidecarManager>();
    let program = sidecar_path(app);
    if !program.exists() {
        return Err(format!(
            "The backend is missing from this build ({}).",
            program.display()
        ));
    }

    let port_arg = port.to_string();
    let mut all_args: Vec<&str> = args.to_vec();
    all_args.extend([
        "--port",
        &port_arg,
        "--no-browser",
        "--exit-with-parent",
        "--desktop",
    ]);
    let mut command = app
        .shell()
        .command(&program)
        .args(&all_args)
        .current_dir(cwd);
    // A repository passed at launch can get here before the login shell has answered.
    match manager.wait_shell_path(SHELL_PATH_WAIT) {
        Some(path) => command = command.env("PATH", path),
        None => log::warn!("starting the backend without the login shell's PATH"),
    }
    let (mut events, child) = command
        .spawn()
        .map_err(|e| format!("Couldn't start the backend: {e}"))?;
    let pid = child.pid();
    log::info!(
        "sidecar {pid} ({slot:?}): {} {}",
        program.display(),
        all_args.join(" ")
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
                                slot,
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

    *manager.slot(slot).lock().unwrap() = Some(Running {
        child,
        pid,
        repo,
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
            *manager.slot(slot).lock().unwrap() = None;
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
    stop_slot(app, slot);
    Err(format!(
        "The backend didn't start listening within {} seconds.",
        STARTUP_TIMEOUT.as_secs()
    ))
}

/// Terminate the review backend, if any. Blocking.
pub fn stop(app: &AppHandle) {
    stop_slot(app, Slot::Review);
}

/// Terminate the Settings window's backend, if any. Blocking.
pub fn stop_settings(app: &AppHandle) {
    stop_slot(app, Slot::Settings);
}

/// SIGTERM first so uvicorn shuts down gracefully (the app's lifespan cleanup stops tsserver
/// and removes snapshot directories); SIGKILL if it hasn't gone within the grace period.
fn stop_slot(app: &AppHandle, slot: Slot) {
    let manager = app.state::<SidecarManager>();
    let running = manager.slot(slot).lock().unwrap().take();
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

/// The backend died on its own while in use. For the review UI: go back to the landing page
/// with the details. For the Settings window: close it and say why.
fn crashed(app: &AppHandle, slot: Slot, pid: u32, how: String, stderr: Vec<String>) {
    let manager = app.state::<SidecarManager>();
    {
        let mut running = manager.slot(slot).lock().unwrap();
        if running.as_ref().map(|r| r.pid) != Some(pid) {
            return; // already replaced
        }
        *running = None;
    }
    let error = LastError {
        title: format!("The backend stopped unexpectedly ({how})."),
        detail: stderr.join("\n"),
    };
    match slot {
        Slot::Review => {
            *manager.last_error.lock().unwrap() = Some(error.clone());
            let _ = app.emit("sidecar-exited", &error);
            crate::commands::show_landing(app, "");
            crate::menu::rebuild(app);
        }
        Slot::Settings => crate::settings_window::backend_crashed(app, &error),
    }
}

/// Capture the login shell's PATH on a worker thread (a slow ~/.zshrc must not block the
/// UI) and remember it for the backend, which needs `gh`, `node`/`tsserver` and system
/// pythons that a GUI app's own PATH doesn't include.
pub fn capture_shell_path(app: AppHandle) {
    std::thread::spawn(move || {
        let path = login_shell_path();
        log::info!("sidecar PATH: {path}");
        app.state::<SidecarManager>().set_shell_path(path);
    });
}

/// Capture it again (the landing page's "Check Again" after installing something). Blocking.
pub fn recapture_shell_path(app: &AppHandle) -> String {
    let path = login_shell_path();
    log::info!("sidecar PATH: {path}");
    app.state::<SidecarManager>().set_shell_path(path.clone());
    path
}

fn login_shell_path() -> String {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    // Interactive rc files may print banners, so bracket the value with markers.
    let from_shell = run_with_timeout(
        std::process::Command::new(&shell)
            .args(["-ilc", "printf '\\n<<PATH>>%s<<END>>\\n' \"$PATH\""]),
        Duration::from_secs(5),
    )
    .and_then(|out| extract_marked_path(&out))
    .unwrap_or_default();
    merge_path(&from_shell, &std::env::var("HOME").unwrap_or_default())
}

/// The value between the last `<<PATH>>` and the `<<END>>` after it.
fn extract_marked_path(out: &str) -> Option<String> {
    let start = out.rfind("<<PATH>>")? + "<<PATH>>".len();
    let end = out[start..].find("<<END>>")? + start;
    Some(out[start..end].trim().to_string())
}

/// The shell's PATH plus the usual tool locations it may lack, without duplicates.
fn merge_path(from_shell: &str, home: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for p in from_shell.split(':').filter(|p| !p.is_empty()) {
        if !parts.iter().any(|q| q == p) {
            parts.push(p.to_string());
        }
    }
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

/// Run `command` and return its stdout if it succeeds within `timeout`.
pub(crate) fn run_with_timeout(
    command: &mut std::process::Command,
    timeout: Duration,
) -> Option<String> {
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
                log::warn!("{command:?} didn't finish within {timeout:?}");
                let _ = child.kill();
                return None;
            }
        }
    }
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
        assert_eq!(choose_port(&[47821, 0], |_| None), None);
    }

    #[test]
    fn marked_path_ignores_banners() {
        assert_eq!(
            extract_marked_path("Welcome!\n<<PATH>>/a:/b<<END>>\n").as_deref(),
            Some("/a:/b")
        );
        // An rc file that echoes the marker itself: the last one wins.
        assert_eq!(
            extract_marked_path("<<PATH>>junk<<END>>\n<<PATH>> /c <<END>>").as_deref(),
            Some("/c")
        );
        assert_eq!(extract_marked_path("<<PATH>>/a"), None);
        assert_eq!(extract_marked_path("nothing"), None);
    }

    #[test]
    fn merged_path_keeps_order_and_adds_extras_once() {
        let merged = merge_path("/x:/usr/bin::/x:/opt/homebrew/bin", "/Users/me");
        let parts: Vec<&str> = merged.split(':').collect();
        assert_eq!(&parts[..3], ["/x", "/usr/bin", "/opt/homebrew/bin"]);
        assert_eq!(parts.iter().filter(|p| **p == "/usr/bin").count(), 1);
        assert!(parts.contains(&"/Users/me/.local/bin"));
        assert!(merge_path("", "").starts_with("/opt/homebrew/bin:"));
    }

    #[test]
    fn shell_path_wait_times_out_then_sees_the_value() {
        let manager = SidecarManager::default();
        assert_eq!(manager.wait_shell_path(Duration::from_millis(10)), None);
        manager.set_shell_path("/bin".into());
        assert_eq!(
            manager
                .wait_shell_path(Duration::from_millis(10))
                .as_deref(),
            Some("/bin")
        );
    }
}
