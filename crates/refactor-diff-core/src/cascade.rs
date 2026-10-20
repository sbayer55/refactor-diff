//! Cascade cleanup: an import or local deleted because its last use was deleted in the same
//! file is re-signed [`SignatureKind::Cascade`](crate::model::SignatureKind::Cascade).
//!
//! Owned by workstream C2 (see docs/concept-lens-plan.md).

use indexmap::IndexMap;

use crate::model::Unit;
use crate::moves::Analyses;

/// Stub: no-op until the workstream lands.
pub fn link(units: &mut IndexMap<String, Unit>, analyses: &Analyses) {
    let _ = units;
    let _ = analyses;
}
