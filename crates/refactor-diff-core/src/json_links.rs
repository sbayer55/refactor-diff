//! JSON-aware patterns: fixture fields dropped at the same path
//! ([`SignatureKind::Fixture`](crate::model::SignatureKind::Fixture)) and i18n keys paired with
//! the `t("key")` calls that used them ([`SignatureKind::I18n`](crate::model::SignatureKind::I18n)).
//!
//! Owned by workstream D (see docs/concept-lens-plan.md).

use indexmap::IndexMap;

use crate::model::{FileSummary, Unit};
use crate::moves::Analyses;

/// Stub: no-op until the workstream lands.
pub fn link(units: &mut IndexMap<String, Unit>, files: &[FileSummary], analyses: &Analyses) {
    let _ = units;
    let _ = files;
    let _ = analyses;
}
