//! The refactor-diff server library: HTTP API, git sources, navigation, AI and persistence.
//!
//! [`App`] wires a repository to the JSON API the SPA talks to; the CLI and the desktop app
//! build one from a [`ServerConfig`] and serve it on a local port.

pub mod ai;
pub mod app;
pub mod config;
pub mod exec;
pub mod git;
pub mod http;
pub mod nav;
pub mod paths;
pub mod prefs;
pub mod review;
pub mod settings;
pub mod snapshots;

pub use app::{App, AppState, BuildError, ServerHandle, bind_local};
pub use config::{
    ConfigError, Defaults, EDITORS, Filters, Mode, ServerConfig, defaults_from, editor_template,
    filter_defaults,
};
