//! Scoring one report in the background: cached hunks are filled in at once, the rest are
//! sent to Jev a few at a time, then the larger mechanical groups are checked the same way
//! ([`super::groups`]), and the UI polls [`JevJob::snapshot`] until it is done.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use refactor_diff_core::Report;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::groups::{self, GroupCheck, GroupSample, MAX_GROUPS_PER_RUN, group_state};
use super::questions::{group_questions, questions};
use super::risk::{HunkRisk, score};
use super::state::hunk_state;
use super::{JevEvaluator, Questions};
use crate::ai::context::ContextBuilder;
use crate::app::AppState;
use crate::http::error::ApiError;

/// Requests in flight at once.
pub const MAX_IN_FLIGHT: usize = 4;
/// Hunks sent per run; the rest wait for the next run.
pub const MAX_HUNKS_PER_RUN: usize = 300;
/// Results are written to the cache every this many records, and at the end.
const FLUSH_EVERY: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum JevStatus {
    Idle,
    /// `total` counts the hunks and the groups sent this run.
    Running {
        done: usize,
        total: usize,
    },
    Done {
        total: usize,
        /// Hunks scored this run and hunks filled from the cache.
        scored: usize,
        cached: usize,
        failed: usize,
        /// Groups checked this run and groups filled from the cache.
        groups_checked: usize,
        groups_cached: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Failure {
    /// For a group, the hunk of its first sampled member.
    pub hunk_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<String>,
    pub error: String,
}

pub struct JevJob {
    status: Mutex<JevStatus>,
    /// By hunk id.
    results: Mutex<HashMap<String, HunkRisk>>,
    /// Scored this run and not yet written to the cache.
    unflushed: Mutex<Vec<HunkRisk>>,
    /// Group checks by group id.
    groups: Mutex<HashMap<String, GroupCheck>>,
    unflushed_groups: Mutex<Vec<GroupCheck>>,
    failures: Mutex<Vec<Failure>>,
    cancel: CancellationToken,
}

impl JevJob {
    fn new() -> Self {
        Self {
            status: Mutex::new(JevStatus::Idle),
            results: Mutex::new(HashMap::new()),
            unflushed: Mutex::new(vec![]),
            groups: Mutex::new(HashMap::new()),
            unflushed_groups: Mutex::new(vec![]),
            failures: Mutex::new(vec![]),
            cancel: CancellationToken::new(),
        }
    }

    pub fn status(&self) -> JevStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn is_running(&self) -> bool {
        matches!(self.status(), JevStatus::Running { .. })
    }

    fn set_status(&self, status: JevStatus) {
        *self.status.lock().unwrap_or_else(|e| e.into_inner()) = status;
    }

    pub fn results(&self) -> HashMap<String, HunkRisk> {
        self.results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Group checks so far, by group id.
    pub fn group_results(&self) -> HashMap<String, GroupCheck> {
        self.groups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `{"status", "hunks": {hunk_id: HunkRisk}, "groups": {group_id: {same_edit, sampled}},
    /// "jev_verified_units", "failures": [...]}`.
    pub fn snapshot(&self, report: &Report) -> Value {
        let failures = self
            .failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let checks = self.group_results();
        json!({
            "status": self.status(),
            "hunks": self.results(),
            "groups": groups::snapshot(&checks),
            "jev_verified_units": groups::verified_units(report, &checks),
            "failures": failures,
        })
    }

    fn bump_done(&self) {
        let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
        if let JevStatus::Running { done, .. } = &mut *status {
            *done += 1;
        }
    }

    fn record(&self, risk: HunkRisk) {
        self.unflushed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(risk.clone());
        self.results
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(risk.hunk_id.clone(), risk);
    }

    fn record_group(&self, check: GroupCheck) {
        self.unflushed_groups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(check.clone());
        self.groups
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(check.group_id.clone(), check);
    }

    /// Record a failure; an auth failure ends the run. False when the run should stop.
    fn failed(&self, failure: Failure) -> bool {
        let fatal = failure.error.contains("HTTP 401") || failure.error.contains("HTTP 403");
        let message = failure.error.clone();
        self.failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(failure);
        if fatal {
            self.set_status(JevStatus::Error { message });
            self.cancel.cancel();
        }
        !fatal
    }

    /// Everything unflushed, once there are at least `min` records in all.
    fn take_unflushed(&self, min: usize) -> (Vec<HunkRisk>, Vec<GroupCheck>) {
        let mut hunks = self.unflushed.lock().unwrap_or_else(|e| e.into_inner());
        let mut groups = self
            .unflushed_groups
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if hunks.len() + groups.len() < min {
            return (vec![], vec![]);
        }
        (std::mem::take(&mut *hunks), std::mem::take(&mut *groups))
    }
}

/// The hunks to send, in order, and how many were left out. With a concept overlay, hunks
/// whose every unit the strongest concept explains go last, and are the ones dropped when
/// there are more than `cap`. Without one, this is a plain cut at `cap`.
pub fn select_hunks(report: &Report, pending: Vec<String>, cap: usize) -> (Vec<String>, usize) {
    let explained: HashSet<&str> = report
        .concepts
        .first()
        .map(|c| c.explained_unit_ids.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let is_explained = |id: &String| {
        !explained.is_empty()
            && report.hunks.get(id).is_some_and(|h| {
                !h.unit_ids.is_empty() && h.unit_ids.iter().all(|u| explained.contains(u.as_str()))
            })
    };
    let total = pending.len();
    let (mut first, last): (Vec<String>, Vec<String>) =
        pending.into_iter().partition(|id| !is_explained(id));
    if total > cap {
        first.truncate(cap);
    } else {
        first.extend(last);
    }
    let left_out = total - first.len();
    (first, left_out)
}

/// Start scoring (or return the job already running for this report). `force` ignores the
/// cache and restarts a running job. Fails with `NeedsSettings` when Jev has no API key.
pub fn start(
    state: Arc<AppState>,
    report: Arc<Report>,
    force: bool,
) -> Result<Arc<JevJob>, ApiError> {
    let evaluator = state
        .jev_factory
        .build(&state.settings.load())
        .map_err(|e| ApiError::NeedsSettings(e.0))?;
    let job = {
        let mut jobs = state.jev.write().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = jobs.get(&report.id) {
            if existing.is_running() && !force {
                return Ok(existing.clone());
            }
            existing.cancel.cancel();
        }
        let job = Arc::new(JevJob::new());
        job.set_status(JevStatus::Running { done: 0, total: 0 });
        jobs.insert(report.id.clone(), job.clone());
        job
    };
    tokio::spawn(run(job.clone(), state, report, evaluator, force));
    Ok(job)
}

async fn run(
    job: Arc<JevJob>,
    state: Arc<AppState>,
    report: Arc<Report>,
    evaluator: Arc<dyn JevEvaluator>,
    force: bool,
) {
    let shutdown = state.shutdown.clone();
    let cancel = job.cancel.clone();
    tokio::select! {
        _ = shutdown.cancelled() => {}
        _ = cancel.cancelled() => {}
        _ = run_inner(job.clone(), state.clone(), report, evaluator, force) => {}
    }
    // Whatever stopped us, keep what was scored.
    flush(&state, &job, 1).await;
    if job.is_running() {
        job.set_status(JevStatus::Error {
            message: "Scoring was interrupted.".into(),
        });
    }
}

async fn run_inner(
    job: Arc<JevJob>,
    state: Arc<AppState>,
    report: Arc<Report>,
    evaluator: Arc<dyn JevEvaluator>,
    force: bool,
) {
    let (cached, cached_groups) = if force {
        (HashMap::new(), HashMap::new())
    } else {
        let st = state.clone();
        tokio::task::spawn_blocking(move || (st.jev_store.load(), st.jev_store.load_groups()))
            .await
            .unwrap_or_default()
    };
    let mut pending: Vec<String> = Vec::new();
    let mut hits = 0;
    for id in &report.residual_hunk_ids {
        let Some(hunk) = report.hunks.get(id) else {
            continue;
        };
        match cached.get(&hunk.fingerprint) {
            Some(r) => {
                hits += 1;
                let mut r = r.clone();
                r.hunk_id = id.clone();
                r.path = hunk.path.clone();
                job.results
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.clone(), r);
            }
            None => pending.push(id.clone()),
        }
    }
    let (pending, left_out) = select_hunks(&report, pending, MAX_HUNKS_PER_RUN);

    let mut pending_groups: Vec<GroupSample> = Vec::new();
    let mut group_hits = 0;
    for sample in groups::plan(&report) {
        match cached_groups.get(&sample.cache_key) {
            Some(c) => {
                group_hits += 1;
                job.groups
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(sample.group_id.clone(), groups::relabel(c, &sample));
            }
            None => pending_groups.push(sample),
        }
    }
    let groups_left_out = pending_groups.len().saturating_sub(MAX_GROUPS_PER_RUN);
    pending_groups.truncate(MAX_GROUPS_PER_RUN);

    let total = pending.len() + pending_groups.len();
    job.set_status(JevStatus::Running { done: 0, total });

    let builder = ContextBuilder::new(report.clone(), state.git.clone(), None);
    let questions: Arc<Questions> = Arc::new(questions());
    let scored = Arc::new(Mutex::new(0usize));
    futures::stream::iter(pending)
        .for_each_concurrent(MAX_IN_FLIGHT, |hunk_id| {
            let (job, state, report, builder, evaluator, questions, scored) = (
                job.clone(),
                state.clone(),
                report.clone(),
                builder.clone(),
                evaluator.clone(),
                questions.clone(),
                scored.clone(),
            );
            async move {
                if job.cancel.is_cancelled() {
                    return;
                }
                match score_one(&report, &builder, &*evaluator, &questions, &hunk_id).await {
                    Ok(risk) => {
                        job.record(risk);
                        *scored.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                    }
                    Err(error) => {
                        let failure = Failure {
                            hunk_id: hunk_id.clone(),
                            group_id: None,
                            error,
                        };
                        if !job.failed(failure) {
                            return;
                        }
                    }
                }
                job.bump_done();
                flush(&state, &job, FLUSH_EVERY).await;
            }
        })
        .await;

    let group_questions: Arc<Questions> = Arc::new(group_questions());
    let checked = Arc::new(Mutex::new(0usize));
    futures::stream::iter(pending_groups)
        .for_each_concurrent(MAX_IN_FLIGHT, |sample| {
            let (job, state, report, builder, evaluator, questions, checked) = (
                job.clone(),
                state.clone(),
                report.clone(),
                builder.clone(),
                evaluator.clone(),
                group_questions.clone(),
                checked.clone(),
            );
            async move {
                if job.cancel.is_cancelled() {
                    return;
                }
                match check_one(&builder, &*evaluator, &questions, &sample).await {
                    Ok(check) => {
                        job.record_group(check);
                        *checked.lock().unwrap_or_else(|e| e.into_inner()) += 1;
                    }
                    Err(error) => {
                        let hunk_id = sample
                            .sampled
                            .first()
                            .and_then(|u| report.units.get(u))
                            .map(|u| u.hunk_id.clone())
                            .unwrap_or_default();
                        let failure = Failure {
                            hunk_id,
                            group_id: Some(sample.group_id.clone()),
                            error,
                        };
                        if !job.failed(failure) {
                            return;
                        }
                    }
                }
                job.bump_done();
                flush(&state, &job, FLUSH_EVERY).await;
            }
        })
        .await;

    flush(&state, &job, 1).await;
    if job.is_running() {
        let scored = *scored.lock().unwrap_or_else(|e| e.into_inner());
        let groups_checked = *checked.lock().unwrap_or_else(|e| e.into_inner());
        let failed = job.failures.lock().unwrap_or_else(|e| e.into_inner()).len();
        let mut notes = vec![];
        if left_out > 0 {
            notes.push(format!(
                "Only the first {MAX_HUNKS_PER_RUN} unscored hunks were sent; score again for the rest."
            ));
        }
        if groups_left_out > 0 {
            notes.push(format!(
                "Only the first {MAX_GROUPS_PER_RUN} groups were checked; score again for the rest."
            ));
        }
        job.set_status(JevStatus::Done {
            total,
            scored,
            cached: hits,
            failed,
            groups_checked,
            groups_cached: group_hits,
            note: (!notes.is_empty()).then(|| notes.join(" ")),
        });
    }
}

async fn score_one(
    report: &Report,
    builder: &ContextBuilder,
    evaluator: &dyn JevEvaluator,
    questions: &Questions,
    hunk_id: &str,
) -> Result<HunkRisk, String> {
    let (b, id) = (builder.clone(), hunk_id.to_string());
    let (state, evidence) = tokio::task::spawn_blocking(move || hunk_state(&b, &id))
        .await
        .map_err(|e| format!("building the hunk's context failed: {e}"))?
        .map_err(|e| e.0)?;
    let eval = evaluator
        .evaluate(state, questions)
        .await
        .map_err(|e| e.0)?;
    let hunk = report
        .hunks
        .get(hunk_id)
        .ok_or_else(|| "Unknown hunk; run the analysis again.".to_string())?;
    Ok(score(hunk, evidence, eval))
}

async fn check_one(
    builder: &ContextBuilder,
    evaluator: &dyn JevEvaluator,
    questions: &Questions,
    sample: &GroupSample,
) -> Result<GroupCheck, String> {
    let (b, s) = (builder.clone(), sample.clone());
    let state = tokio::task::spawn_blocking(move || group_state(&b, &s))
        .await
        .map_err(|e| format!("building the group's context failed: {e}"))?
        .map_err(|e| e.0)?;
    let eval = evaluator
        .evaluate(state, questions)
        .await
        .map_err(|e| e.0)?;
    Ok(groups::check(sample, eval))
}

/// Write the unflushed results to the cache once there are at least `min` of them.
async fn flush(state: &Arc<AppState>, job: &Arc<JevJob>, min: usize) {
    let (hunks, groups) = job.take_unflushed(min);
    if hunks.is_empty() && groups.is_empty() {
        return;
    }
    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || st.jev_store.put_all(&hunks, &groups)).await;
    match result {
        Ok(Err(e)) => tracing::warn!("couldn't write the Jev cache: {e}"),
        Err(e) => tracing::warn!("writing the Jev cache panicked: {e}"),
        Ok(Ok(())) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;
    use refactor_diff_core::{Concept, Hunk, Source};

    fn hunk(id: &str, units: &[&str]) -> Hunk {
        Hunk {
            id: id.into(),
            path: "a.ts".into(),
            old_start: 1,
            new_start: 1,
            lines: vec![],
            unit_ids: units.iter().map(|u| u.to_string()).collect(),
            fingerprint: format!("fp-{id}"),
        }
    }

    fn report(explained: &[&str]) -> Report {
        let hunks: IndexMap<String, Hunk> = [
            hunk("h1", &["u1"]),
            hunk("h2", &["u2", "u3"]),
            hunk("h3", &["u4"]),
            hunk("h4", &[]),
        ]
        .into_iter()
        .map(|h| (h.id.clone(), h))
        .collect();
        let concepts = if explained.is_empty() {
            vec![]
        } else {
            vec![Concept {
                stem: "program".into(),
                auto: true,
                score: 0.7,
                explained_unit_ids: explained.iter().map(|u| u.to_string()).collect(),
                touched_unit_ids: vec![],
                patterns: vec![],
                files: IndexMap::new(),
            }]
        };
        Report {
            id: "r".into(),
            source: Source {
                label: "x".into(),
                base: "main".into(),
                head: "feature".into(),
                base_sha: "0".into(),
                head_sha: None,
                pr: None,
                identity: "refs:main:feature".into(),
                min_count: 2,
            },
            files: vec![],
            groups: vec![],
            units: IndexMap::new(),
            residual_hunk_ids: hunks.keys().cloned().collect(),
            hunks,
            warnings: vec![],
            concepts,
            texts: Default::default(),
        }
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn without_a_concept_the_order_is_kept_and_cut() {
        let all = ids(&["h1", "h2", "h3", "h4"]);
        let r = report(&[]);
        assert_eq!(select_hunks(&r, all.clone(), 10), (all.clone(), 0));
        assert_eq!(select_hunks(&r, all, 3), (ids(&["h1", "h2", "h3"]), 1));
    }

    #[test]
    fn concept_explained_hunks_go_last_and_are_dropped_first() {
        let all = ids(&["h1", "h2", "h3", "h4"]);
        // h1 is fully explained; h2 only half; h4 has no units and is never "explained".
        let r = report(&["u1", "u2"]);
        assert_eq!(
            select_hunks(&r, all.clone(), 10),
            (ids(&["h2", "h3", "h4", "h1"]), 0)
        );
        // Over the cap: the explained hunks are skipped entirely.
        let r = report(&["u1", "u4"]);
        assert_eq!(select_hunks(&r, all.clone(), 3), (ids(&["h2", "h4"]), 2));
        assert_eq!(select_hunks(&r, all, 1), (ids(&["h2"]), 3));
    }
}
