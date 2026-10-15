//! Scoring one report in the background: cached hunks are filled in at once, the rest are
//! sent to Jev a few at a time, and the UI polls [`JevJob::snapshot`] until it is done.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use refactor_diff_core::Report;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::questions::questions;
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
/// Results are written to the cache every this many hunks, and at the end.
const FLUSH_EVERY: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "lowercase")]
pub enum JevStatus {
    Idle,
    Running {
        done: usize,
        total: usize,
    },
    Done {
        total: usize,
        scored: usize,
        cached: usize,
        failed: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    Error {
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Failure {
    pub hunk_id: String,
    pub error: String,
}

pub struct JevJob {
    status: Mutex<JevStatus>,
    /// By hunk id.
    results: Mutex<HashMap<String, HunkRisk>>,
    /// Scored this run and not yet written to the cache.
    unflushed: Mutex<Vec<HunkRisk>>,
    failures: Mutex<Vec<Failure>>,
    cancel: CancellationToken,
}

impl JevJob {
    fn new() -> Self {
        Self {
            status: Mutex::new(JevStatus::Idle),
            results: Mutex::new(HashMap::new()),
            unflushed: Mutex::new(vec![]),
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

    /// `{"status", "hunks": {hunk_id: HunkRisk}, "failures": [...]}`.
    pub fn snapshot(&self) -> Value {
        let failures = self
            .failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        json!({
            "status": self.status(),
            "hunks": self.results(),
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

    fn take_unflushed(&self, min: usize) -> Vec<HunkRisk> {
        let mut pending = self.unflushed.lock().unwrap_or_else(|e| e.into_inner());
        if pending.len() < min {
            return vec![];
        }
        std::mem::take(&mut *pending)
    }
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
    let cached = if force {
        HashMap::new()
    } else {
        let st = state.clone();
        tokio::task::spawn_blocking(move || st.jev_store.load())
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
    let capped = pending.len() > MAX_HUNKS_PER_RUN;
    pending.truncate(MAX_HUNKS_PER_RUN);
    let total = pending.len();
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
                        let fatal = error.contains("HTTP 401") || error.contains("HTTP 403");
                        job.failures
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(Failure {
                                hunk_id: hunk_id.clone(),
                                error: error.clone(),
                            });
                        if fatal {
                            job.set_status(JevStatus::Error { message: error });
                            job.cancel.cancel();
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
        let failed = job.failures.lock().unwrap_or_else(|e| e.into_inner()).len();
        job.set_status(JevStatus::Done {
            total,
            scored,
            cached: hits,
            failed,
            note: capped.then(|| {
                format!("Only the first {MAX_HUNKS_PER_RUN} unscored hunks were sent; score again for the rest.")
            }),
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

/// Write the unflushed results to the cache once there are at least `min` of them.
async fn flush(state: &Arc<AppState>, job: &Arc<JevJob>, min: usize) {
    let batch = job.take_unflushed(min);
    if batch.is_empty() {
        return;
    }
    let st = state.clone();
    let result = tokio::task::spawn_blocking(move || st.jev_store.put(&batch)).await;
    match result {
        Ok(Err(e)) => tracing::warn!("couldn't write the Jev cache: {e}"),
        Err(e) => tracing::warn!("writing the Jev cache panicked: {e}"),
        Ok(Ok(())) => {}
    }
}
