//! Tell renames apart from substitutions: `a → b` where `b` already existed in the base is one
//! concept standing in for another, not a rename. Re-signs such units as
//! [`SignatureKind::Substitution`](crate::model::SignatureKind::Substitution).
//!
//! Owned by workstream B (see docs/concept-lens-plan.md).

use indexmap::IndexMap;

use crate::model::Unit;
use crate::moves::Analyses;

/// Stub: no-op until the workstream lands.
pub fn mark(units: &mut IndexMap<String, Unit>, analyses: &Analyses) {
    let _ = units;
    let _ = analyses;
}
