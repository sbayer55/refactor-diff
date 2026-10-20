//! Directory renames and the barrel (`index.ts` `export *`) edits that follow them.
//!
//! Owned by workstream C2 (see docs/concept-lens-plan.md).

use indexmap::IndexMap;

use crate::model::{FileSummary, Unit};
use crate::moves::Analyses;

/// Stub: no-op until the workstream lands.
pub fn link(units: &mut IndexMap<String, Unit>, files: &[FileSummary], analyses: &Analyses) {
    let _ = units;
    let _ = files;
    let _ = analyses;
}
