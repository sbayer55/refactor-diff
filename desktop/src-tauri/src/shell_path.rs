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
                tracing::warn!("{command:?} didn't finish within {timeout:?}");
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
}
