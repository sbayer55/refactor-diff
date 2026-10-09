//! The PATH of the user's login shell.
//!
//! A GUI app launched from Finder only sees `/usr/bin:/bin:/usr/sbin:/sbin`, but the backend
//! spawns `git`, `gh`, `node` (tsserver) and `python`, which usually live in Homebrew, nvm or
//! a version manager that the user's shell rc files put on PATH. The capture runs once at
//! startup on a thread; the first repository open waits (briefly) for it.

use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The login shell's PATH plus the usual tool directories, as one `:`-joined string.
pub fn login_shell_path() -> String {
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
        stdout.read_to_string(&mut buf).ok();
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return reader.join().ok(),
            Ok(Some(_)) => return None,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                tracing::warn!("login shell didn't report PATH within {timeout:?}");
                let _ = child.kill();
                return None;
            }
        }
    }
}
