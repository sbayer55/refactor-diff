//! Child processes. Every external tool (git, gh, node, python) is started through here so
//! the desktop app can hand the server the PATH of the user's login shell.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// How to find and start external tools.
#[derive(Clone, Debug, Default)]
pub struct Tools {
    /// PATH for child processes and lookups; `None` inherits this process's PATH.
    pub path: Option<OsString>,
}

impl Tools {
    pub fn new(path: Option<OsString>) -> Self {
        Self { path }
    }

    /// A command for `program`, with the configured PATH applied.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);
        if let Some(path) = &self.path {
            cmd.env("PATH", path);
        }
        cmd
    }

    /// Where `program` resolves on the configured PATH.
    pub fn which(&self, program: &str) -> Option<PathBuf> {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match &self.path {
            Some(path) => which::which_in(program, Some(path), cwd).ok(),
            None => which::which(program).ok(),
        }
    }

    /// The PATH child processes see.
    pub fn path_var(&self) -> OsString {
        self.path
            .clone()
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default()
    }

    pub fn exists(&self, program: &Path) -> bool {
        program.is_file()
    }
}
