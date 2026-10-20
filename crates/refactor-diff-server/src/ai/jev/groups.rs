//! Verifying mechanical groups: a few members of each larger group are shown to Jev side by
//! side and it is asked whether they are all the same edit. The answer is a hint kept next to
//! the risk scores (`jev_verified_units`); it never touches `Unit.verified` or the report.

use std::collections::{BTreeSet, HashMap};

use refactor_diff_core::{Group, Report, short_hash};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::questions::GROUP_QUESTION_SET_VERSION;
use super::state::stated_change;
use super::{Answer, Evaluation, Usage};
use crate::ai::context::{ContextBuilder, ContextError};
use crate::paths::now_iso;

/// Only groups with at least this many units are checked.
pub const MIN_GROUP_UNITS: usize = 3;
/// Members sampled per group: the first, the middle and the last.
pub const SAMPLES_PER_GROUP: usize = 3;
/// Groups checked per run.
pub const MAX_GROUPS_PER_RUN: usize = 100;
/// A group whose `same_edit` is at least this counts towards `jev_verified_units`.
pub const VERIFIED_P: f64 = 0.8;

/// One group to check: which members are shown and the cache key they make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSample {
    pub group_id: String,
    /// `group.key` plus the sorted fingerprints of the sampled members' hunks.
    pub cache_key: String,
    pub sampled: Vec<String>,
}

/// What the UI and the cache keep for one checked group.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GroupCheck {
    pub cache_key: String,
    pub group_id: String,
    /// p(every sampled occurrence is the same mechanical edit).
    pub same_edit: f64,
    /// The unit ids that were shown.
    pub sampled: Vec<String>,
    pub answers: std::collections::BTreeMap<String, Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
    pub question_set: String,
    pub scored_at: String,
}

/// First, middle and last indexes of `len` items, without repeats.
fn sample_indexes(len: usize) -> Vec<usize> {
    let picks: BTreeSet<usize> = [0, len / 2, len.saturating_sub(1)]
        .into_iter()
        .filter(|i| *i < len)
        .collect();
    picks.into_iter().take(SAMPLES_PER_GROUP).collect()
}

/// The groups worth checking, in report order, with their sampled members.
pub fn plan(report: &Report) -> Vec<GroupSample> {
    report
        .groups
        .iter()
        .filter(|g| g.mechanical && g.unit_ids.len() >= MIN_GROUP_UNITS)
        .filter_map(|g| sample(report, g))
        .collect()
}

fn sample(report: &Report, group: &Group) -> Option<GroupSample> {
    let sampled: Vec<String> = sample_indexes(group.unit_ids.len())
        .into_iter()
        .map(|i| group.unit_ids[i].clone())
        .collect();
    let mut fingerprints: Vec<&str> = sampled
        .iter()
        .map(|uid| {
            let unit = report.units.get(uid)?;
            report
                .hunks
                .get(&unit.hunk_id)
                .map(|h| h.fingerprint.as_str())
        })
        .collect::<Option<_>>()?;
    fingerprints.sort_unstable();
    let cache_key = short_hash(
        std::iter::once(group.key.as_str())
            .chain(fingerprints)
            .collect::<Vec<_>>(),
    );
    Some(GroupSample {
        group_id: group.id.clone(),
        cache_key,
        sampled,
    })
}

/// The state for one group. Parses nothing heavy, but reads the report's hunks; call it off
/// the async threads with the hunk states.
pub fn group_state(builder: &ContextBuilder, sample: &GroupSample) -> Result<Value, ContextError> {
    let report = builder.report();
    let group = report
        .groups
        .iter()
        .find(|g| g.id == sample.group_id)
        .ok_or_else(|| ContextError::new("Unknown group; run the analysis again."))?;
    let mut occurrences = Vec::with_capacity(sample.sampled.len());
    for uid in &sample.sampled {
        let unit = report
            .units
            .get(uid)
            .ok_or_else(|| ContextError::new("Unknown unit; run the analysis again."))?;
        let focus = builder.focus(&unit.hunk_id, None, None)?;
        occurrences.push(Value::String(builder.hunk_piece(&focus)));
    }
    let mut state = json!({
        "pattern": {
            "kind": group.kind.as_str(),
            "label": group.label,
            "old": group.old,
            "new": group.new,
        },
        "occurrences": occurrences,
    });
    if let Some(stated) = stated_change(builder) {
        state["stated_change"] = Value::String(stated);
    }
    Ok(state)
}

/// Fold an evaluation into the group's record.
pub fn check(sample: &GroupSample, eval: Evaluation) -> GroupCheck {
    let same_edit = match eval.answers.get("same_edit") {
        Some(Answer::Boolean { probability }) if !probability.is_nan() => {
            probability.clamp(0.0, 1.0)
        }
        _ => 0.0,
    };
    GroupCheck {
        cache_key: sample.cache_key.clone(),
        group_id: sample.group_id.clone(),
        same_edit,
        sampled: sample.sampled.clone(),
        answers: eval.answers,
        usage: eval.usage,
        cost: eval.cost,
        question_set: GROUP_QUESTION_SET_VERSION.to_string(),
        scored_at: now_iso(),
    }
}

/// A cached record re-labelled for this report's group and sample.
pub fn relabel(cached: &GroupCheck, sample: &GroupSample) -> GroupCheck {
    GroupCheck {
        group_id: sample.group_id.clone(),
        sampled: sample.sampled.clone(),
        ..cached.clone()
    }
}

/// The cached checks for this report's groups, by group id.
pub fn from_cache(
    report: &Report,
    cached: &HashMap<String, GroupCheck>,
) -> HashMap<String, GroupCheck> {
    plan(report)
        .iter()
        .filter_map(|s| {
            cached
                .get(&s.cache_key)
                .map(|c| (s.group_id.clone(), relabel(c, s)))
        })
        .collect()
}

/// Units in groups Jev judged to be one edit (`same_edit` ≥ [`VERIFIED_P`]).
pub fn verified_units(report: &Report, checks: &HashMap<String, GroupCheck>) -> usize {
    let units: BTreeSet<&str> = report
        .groups
        .iter()
        .filter(|g| checks.get(&g.id).is_some_and(|c| c.same_edit >= VERIFIED_P))
        .flat_map(|g| g.unit_ids.iter().map(String::as_str))
        .collect();
    units.len()
}

/// `{group_id: {same_edit, sampled}}` for the snapshot.
pub fn snapshot(checks: &HashMap<String, GroupCheck>) -> Value {
    let map: serde_json::Map<String, Value> = checks
        .iter()
        .map(|(id, c)| {
            (
                id.clone(),
                json!({"same_edit": c.same_edit, "sampled": c.sampled}),
            )
        })
        .collect();
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_first_middle_last() {
        assert_eq!(sample_indexes(0), Vec::<usize>::new());
        assert_eq!(sample_indexes(1), vec![0]);
        assert_eq!(sample_indexes(2), vec![0, 1]);
        assert_eq!(sample_indexes(3), vec![0, 1, 2]);
        assert_eq!(sample_indexes(10), vec![0, 5, 9]);
    }

    #[test]
    fn same_edit_is_clamped_and_defaults_to_zero() {
        let sample = GroupSample {
            group_id: "g1".into(),
            cache_key: "k".into(),
            sampled: vec!["u1".into()],
        };
        let eval = super::super::parse_evaluation(
            &json!({"answers": {"same_edit": {"type": "boolean", "probability": 1.2}}}),
        )
        .unwrap();
        let c = check(&sample, eval);
        assert_eq!(c.same_edit, 1.0);
        assert_eq!(c.question_set, GROUP_QUESTION_SET_VERSION);
        let c = check(&sample, Evaluation::default());
        assert_eq!(c.same_edit, 0.0);
        let other = GroupSample {
            group_id: "g2".into(),
            sampled: vec!["u9".into()],
            ..sample
        };
        let r = relabel(&c, &other);
        assert_eq!((r.group_id.as_str(), r.sampled.len()), ("g2", 1));
        assert_eq!(r.cache_key, "k");
    }
}
