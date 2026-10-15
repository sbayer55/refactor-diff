//! Folding Jev's answers into one risk record per hunk. Every threshold and weight lives
//! here so the ranking can be tuned in one place.

use std::collections::BTreeMap;

use refactor_diff_core::Hunk;
use serde::{Deserialize, Serialize};

use super::questions::{
    BLAST_RADIUS, BOOLEAN_CONCERNS, QUESTION_SET_VERSION, REVIEWER_EFFORT, rung_label,
};
use super::state::Evidence;
use super::{Answer, Evaluation, Usage};
use crate::paths::now_iso;

/// Any concern at or above this probability flags the hunk.
pub const FLAG_BOOLEAN_P: f64 = 0.8;
/// A primary concern other than `none`/`insufficient_evidence` flags the hunk at this
/// confidence.
pub const FLAG_CONCERN_CONFIDENCE: f64 = 0.6;
/// Weights of the composite `risk`.
pub const W_CONCERN: f64 = 0.6;
pub const W_SCOPE: f64 = 0.4;
/// `level` cut-offs on `risk`.
pub const LEVEL_HIGH: f64 = 0.6;
pub const LEVEL_MEDIUM: f64 = 0.3;

const NO_CONCERN: [&str; 2] = ["none", "insufficient_evidence"];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Concern {
    pub name: String,
    pub p: f64,
}

/// What the UI and the cache keep for one scored hunk.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HunkRisk {
    pub fingerprint: String,
    pub hunk_id: String,
    pub path: String,
    /// 0..1 composite of the concerns and the scope.
    pub risk: f64,
    /// `low`, `medium` or `high`.
    pub level: String,
    pub flagged: bool,
    pub primary_concern: String,
    /// The probability of the chosen primary concern.
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_up: Option<Concern>,
    pub blast_radius: f64,
    pub blast_label: String,
    pub reviewer_effort: f64,
    pub effort_label: String,
    /// The yes/no concerns, highest first.
    pub concerns: Vec<Concern>,
    pub adds_tests: bool,
    pub docs_only: bool,
    /// Every answer as Jev gave it.
    pub answers: BTreeMap<String, Answer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
    pub question_set: String,
    pub scored_at: String,
}

fn clamp01(x: f64) -> f64 {
    if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }
}

fn boolean_p(answers: &BTreeMap<String, Answer>, name: &str) -> f64 {
    match answers.get(name) {
        Some(Answer::Boolean { probability }) => clamp01(*probability),
        _ => 0.0,
    }
}

fn score_of(answers: &BTreeMap<String, Answer>, name: &str, max: f64) -> f64 {
    match answers.get(name) {
        Some(Answer::Score { score, .. }) if score.is_finite() => score.clamp(0.0, max),
        _ => 0.0,
    }
}

/// Fold an evaluation into the hunk's risk record.
pub fn score(hunk: &Hunk, evidence: Evidence, eval: Evaluation) -> HunkRisk {
    score_at(hunk, evidence, eval, now_iso())
}

pub fn score_at(hunk: &Hunk, evidence: Evidence, eval: Evaluation, scored_at: String) -> HunkRisk {
    let answers = eval.answers;
    let mut concerns: Vec<Concern> = BOOLEAN_CONCERNS
        .iter()
        .map(|name| Concern {
            name: name.to_string(),
            p: boolean_p(&answers, name),
        })
        .collect();
    concerns.sort_by(|a, b| b.p.partial_cmp(&a.p).unwrap_or(std::cmp::Ordering::Equal));
    let concern_max = concerns.first().map_or(0.0, |c| c.p);

    let blast_max = (BLAST_RADIUS.len() - 1) as f64;
    let effort_max = (REVIEWER_EFFORT.len() - 1) as f64;
    let blast = score_of(&answers, "blast_radius", blast_max);
    let effort = score_of(&answers, "reviewer_effort", effort_max);
    let scope = (blast / blast_max) * (0.5 + 0.5 * (effort / effort_max));
    let risk = clamp01(W_CONCERN * concern_max + W_SCOPE * scope);

    let (primary, confidence, runner_up) = match answers.get("primary_concern") {
        Some(Answer::Choice {
            choice,
            probabilities,
        }) => {
            let mut ranked: Vec<(&String, f64)> = probabilities
                .iter()
                .map(|(k, v)| (k, clamp01(*v)))
                .collect();
            ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let confidence = probabilities
                .get(choice)
                .map(|p| clamp01(*p))
                .or_else(|| ranked.first().map(|r| r.1))
                .unwrap_or(0.0);
            let runner_up = ranked
                .iter()
                .find(|(name, _)| *name != choice)
                .map(|(name, p)| Concern {
                    name: name.to_string(),
                    p: *p,
                });
            (choice.clone(), confidence, runner_up)
        }
        _ => ("insufficient_evidence".to_string(), 0.0, None),
    };

    let flagged = concern_max >= FLAG_BOOLEAN_P
        || (!NO_CONCERN.contains(&primary.as_str()) && confidence >= FLAG_CONCERN_CONFIDENCE);
    let level = if flagged && risk >= LEVEL_HIGH {
        "high"
    } else if flagged || risk >= LEVEL_MEDIUM {
        "medium"
    } else {
        "low"
    };

    HunkRisk {
        fingerprint: hunk.fingerprint.clone(),
        hunk_id: hunk.id.clone(),
        path: hunk.path.clone(),
        risk,
        level: level.to_string(),
        flagged,
        primary_concern: primary,
        confidence,
        runner_up,
        blast_radius: blast,
        blast_label: rung_label(&BLAST_RADIUS, blast.round() as usize),
        reviewer_effort: effort,
        effort_label: rung_label(&REVIEWER_EFFORT, effort.round() as usize),
        concerns,
        adds_tests: evidence.adds_tests,
        docs_only: evidence.docs_only,
        answers,
        usage: eval.usage,
        cost: eval.cost,
        question_set: QUESTION_SET_VERSION.to_string(),
        scored_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hunk() -> Hunk {
        Hunk {
            id: "h1".into(),
            path: "api/auth.py".into(),
            old_start: 1,
            new_start: 1,
            lines: vec![],
            unit_ids: vec![],
            fingerprint: "fp1".into(),
        }
    }

    fn eval(answers: serde_json::Value) -> Evaluation {
        super::super::parse_evaluation(&json!({"answers": answers})).unwrap()
    }

    fn evidence() -> Evidence {
        Evidence {
            adds_tests: false,
            docs_only: false,
        }
    }

    #[test]
    fn a_secret_heavy_hunk_is_high_and_flagged() {
        let r = score_at(
            &hunk(),
            evidence(),
            eval(json!({
                "hardcoded_secret": {"type": "boolean", "probability": 0.96},
                "injection_risk": {"type": "boolean", "probability": 0.1},
                "touches_auth": {"type": "boolean", "probability": 0.99},
                "weakens_tests": {"type": "boolean", "probability": 0.0},
                "breaks_api": {"type": "boolean", "probability": 0.2},
                "data_migration": {"type": "boolean", "probability": 0.0},
                "debug_leftovers": {"type": "boolean", "probability": 0.3},
                "blast_radius": {"type": "score", "score": 3.0, "probabilities": {"3": 1}},
                "reviewer_effort": {"type": "score", "score": 2.0, "probabilities": {"2": 1}},
                "primary_concern": {"type": "choice", "choice": "secret", "probabilities": {"secret": 0.69, "auth": 0.25, "none": 0.06}}
            })),
            "2026-10-14T00:00:00+00:00".into(),
        );
        assert!(r.flagged);
        assert_eq!(r.level, "high");
        assert!((r.risk - (0.6 * 0.99 + 0.4 * 1.0)).abs() < 1e-9);
        assert_eq!(r.primary_concern, "secret");
        assert_eq!(r.confidence, 0.69);
        assert_eq!(r.runner_up.as_ref().unwrap().name, "auth");
        assert_eq!(r.concerns[0].name, "touches_auth");
        assert_eq!(r.concerns[1].name, "hardcoded_secret");
        assert_eq!(r.concerns.len(), 7);
        assert_eq!(
            (r.blast_label.as_str(), r.effort_label.as_str()),
            ("core", "expert")
        );
        assert_eq!(r.question_set, QUESTION_SET_VERSION);
        assert_eq!(r.scored_at, "2026-10-14T00:00:00+00:00");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["fingerprint"], "fp1");
        assert_eq!(v["answers"]["primary_concern"]["choice"], "secret");
        assert_eq!(serde_json::from_value::<HunkRisk>(v).unwrap(), r);
    }

    #[test]
    fn a_quiet_hunk_is_low() {
        let r = score_at(
            &hunk(),
            Evidence {
                adds_tests: true,
                docs_only: false,
            },
            eval(json!({
                "hardcoded_secret": {"type": "boolean", "probability": 0.01},
                "touches_auth": {"type": "boolean", "probability": 0.02},
                "blast_radius": {"type": "score", "score": 1.0, "probabilities": {}},
                "reviewer_effort": {"type": "score", "score": 0.2, "probabilities": {}},
                "primary_concern": {"type": "choice", "choice": "none", "probabilities": {"none": 0.9, "logic": 0.1}}
            })),
            "t".into(),
        );
        assert!(!r.flagged);
        assert_eq!(r.level, "low");
        assert!(r.risk < LEVEL_MEDIUM);
        assert!(r.adds_tests);
        assert_eq!(r.blast_label, "local");
        assert_eq!(r.effort_label, "glance");
        // Missing booleans count as 0, not as missing.
        assert_eq!(r.concerns.len(), 7);
        assert!(
            r.concerns
                .iter()
                .any(|c| c.name == "weakens_tests" && c.p == 0.0)
        );
    }

    #[test]
    fn thresholds_at_the_boundary() {
        let base = |p: f64, concern: &str, conf: f64| {
            eval(json!({
                "breaks_api": {"type": "boolean", "probability": p},
                "blast_radius": {"type": "score", "score": 0.0, "probabilities": {}},
                "reviewer_effort": {"type": "score", "score": 0.0, "probabilities": {}},
                "primary_concern": {"type": "choice", "choice": concern, "probabilities": {concern: conf}}
            }))
        };
        let r = score_at(&hunk(), evidence(), base(0.8, "none", 1.0), "t".into());
        assert!(r.flagged && r.level == "medium");
        let r = score_at(&hunk(), evidence(), base(0.79, "none", 1.0), "t".into());
        assert!(!r.flagged && r.level == "medium"); // 0.6 * 0.79 = 0.474 >= 0.3
        let r = score_at(
            &hunk(),
            evidence(),
            base(0.1, "api_contract", 0.6),
            "t".into(),
        );
        assert!(r.flagged && r.level == "medium");
        let r = score_at(
            &hunk(),
            evidence(),
            base(0.1, "api_contract", 0.59),
            "t".into(),
        );
        assert!(!r.flagged && r.level == "low");
        let r = score_at(
            &hunk(),
            evidence(),
            base(0.1, "insufficient_evidence", 0.99),
            "t".into(),
        );
        assert!(!r.flagged);
        // No primary concern answer at all.
        let r = score_at(&hunk(), evidence(), eval(json!({})), "t".into());
        assert_eq!(r.primary_concern, "insufficient_evidence");
        assert_eq!(r.risk, 0.0);
        assert_eq!(r.blast_label, "none");
    }
}
