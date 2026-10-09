//! The server's code navigator as a [`ReferenceSource`] for the context builder.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use super::context::{RefLocation, ReferenceSource};
use crate::app::AppState;
use crate::nav::Kind;

/// Looks references up through `AppState::navigator`, keeping repository hits only.
pub struct NavigatorSource {
    state: Arc<AppState>,
}

impl NavigatorSource {
    /// The server's navigator as a shared reference source.
    pub fn source(state: Arc<AppState>) -> Arc<dyn ReferenceSource> {
        Arc::new(Self { state })
    }
}

impl ReferenceSource for NavigatorSource {
    fn references(
        &self,
        sha: Option<&str>,
        path: &str,
        line: u32,
        col: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RefLocation>, String>> + Send + '_>> {
        let sha = sha.map(String::from);
        let path = path.to_string();
        Box::pin(async move {
            let locations = self
                .state
                .navigator
                .references(sha.as_deref(), &path, line, col)
                .await
                .map_err(|e| e.0)?;
            Ok(locations
                .into_iter()
                .filter(|loc| loc.kind == Kind::Repo)
                .map(|loc| RefLocation {
                    path: loc.path,
                    line: loc.line,
                    text: loc.text,
                    is_definition: loc.is_definition,
                })
                .collect())
        })
    }
}
