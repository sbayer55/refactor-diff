//! Whole test blocks (`it`/`test`/`describe`) deleted or added: tagged
//! [`UnitTag::TestRemoved`](crate::model::UnitTag::TestRemoved) and signed
//! [`SignatureKind::Test`](crate::model::SignatureKind::Test).
//!
//! Owned by workstream C2 (see docs/concept-lens-plan.md).

use indexmap::IndexMap;

use crate::model::Unit;
use crate::moves::Analyses;

/// Stub: no-op until the workstream lands.
pub fn tag(units: &mut IndexMap<String, Unit>, analyses: &Analyses) {
    let _ = units;
    let _ = analyses;
}
