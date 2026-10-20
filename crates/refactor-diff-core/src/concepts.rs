//! The concept lens: identifier stems a change set adds or removes everywhere (`program`), the
//! units that removing the concept fully explains, and the mechanical groups that are the same
//! concept edit once literals are normalized. An overlay: never touches `Unit::explained`.
//!
//! Owned by workstream A (see docs/concept-lens-plan.md).

use indexmap::IndexMap;

use crate::engine::AnalyzeOptions;
use crate::model::{Concept, FileSummary, Group, Unit};
use crate::moves::Analyses;

/// Stub: no concepts until the workstream lands.
pub fn detect(
    units: &IndexMap<String, Unit>,
    groups: &[Group],
    files: &[FileSummary],
    analyses: &Analyses,
    opts: &AnalyzeOptions,
) -> Vec<Concept> {
    let _ = (units, groups, files, analyses, opts);
    Vec::new()
}
