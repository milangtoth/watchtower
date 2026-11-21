use crate::{
    db::{Job, OutageLimit, Store},
    discovery,
    domain::{
        CandidateRead, ConnectionDecision, ConnectionMethod, FieldChange, MovedFile,
        PublishedTender, ReadFailure, Review, StoredEvidence, Tender, TenderStatus, TenderSummary,
    },
    fetch::Fetcher,
    model::{PortalModel, ReasoningContext},
    recipe::{self, Recipe},
};
use anyhow::{Context, Result};
use reqwest::Url;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, Notify},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

type ActiveWork = HashMap<(String, i64, String), CancellationToken>;

#[derive(Clone, Default)]
pub struct Jobs {
    wake: Arc<Notify>,
    active: Arc<Mutex<ActiveWork>>,
}

impl Jobs {
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub async fn cancel(&self, id: String, generation: i64) {
        for ((portal, version, _), token) in self.active.lock().await.iter() {
            if portal == &id && *version == generation {
                token.cancel();
            }
        }
    }
}

#[derive(Clone)]
pub struct Worker {
    pub store: Store,
    pub jobs: Jobs,
    pub fetcher: Fetcher,
    pub model: Arc<dyn PortalModel>,
    /// Seconds between regular reads of a portal.
    pub interval: i64,
    pub recovery: Recovery,
    /// Portals worked on at once. Each portal keeps its own one-request-per-second limit, so
    /// this only bounds total load, not the load on any one portal.
    pub concurrency: usize,
}

/// Default for `Worker::concurrency`.
pub const CONCURRENCY: usize = 32;

/// How a portal recovers from failures without a person. All durations are in seconds.
#[derive(Clone, Debug)]
pub struct Recovery {
    /// Waits after the first, second and third consecutive temporary failure. Later retries
    /// wait the regular interval.
    pub backoff: [i64; 3],
    /// The longest `Retry-After` honored.
    pub retry_after_cap: i64,
    /// Consecutive temporary failures that make a portal failed.
    pub outage_failures: i64,
    /// Time without a successful read that makes a portal failed during temporary failures.
    pub outage_seconds: i64,
    /// Least time between two paid repairs that ran out of attempts. A manual refresh may
    /// repair sooner.
    pub repair_cooldown: i64,
    /// Wait before reading a portal that blocked delivery with a login or CAPTCHA, or whose
    /// attach failed, again.
    pub recheck: i64,
}

impl Default for Recovery {
    fn default() -> Self {
        Self {
            backoff: [60, 300, 900],
            retry_after_cap: 3600,
            outage_failures: 3,
            outage_seconds: 3600,
            repair_cooldown: 6 * 3600,
            recheck: 6 * 3600,
        }
    }
}

impl Worker {
    pub async fn run(self, stop: CancellationToken) {
        let mut running = JoinSet::new();
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                biased;
                _ = stop.cancelled() => break,
                Some(result) = running.join_next(), if !running.is_empty() => {
                    if let Err(error) = result { tracing::error!(%error, "Portal task exited unexpectedly"); }
                }
                _ = self.jobs.wake.notified() => {},
                _ = tick.tick() => {},
            }
            if let Err(error) = self.store.enqueue_due().await {
                tracing::error!(%error, "Could not enqueue scheduled reads");
                continue;
            }
            while running.len() < self.concurrency.max(1) && !stop.is_cancelled() {
                let job = match self.store.claim().await {
                    Ok(Some(job)) => job,
                    Ok(None) => break,
                    Err(error) => {
                        tracing::error!(%error, "Could not claim portal work");
                        break;
                    }
                };
                let token = stop.child_token();
                let key = (job.portal_id.clone(), job.generation, job.id.clone());
                self.jobs
                    .active
                    .lock()
                    .await
                    .insert(key.clone(), token.clone());
                let worker = self.clone();
                running.spawn(async move {
                    tokio::select! {
                        biased;
                        _ = token.cancelled() => {},
                        result = worker.execute(&job) => {
                            if let Err(error) = result {
                                tracing::error!(%error, portal_id = %job.portal_id, "Portal worker failed");
                                if let Err(error) = worker.store.fail_job(&job, "Internal error while reading the portal", worker.interval).await {
                                    tracing::error!(%error, "Could not record worker failure");
                                }
                            }
                        }
                    }
                    worker.jobs.active.lock().await.remove(&key);
                });
            }
        }
        while running.join_next().await.is_some() {}
    }

    async fn execute(&self, job: &Job) -> Result<()> {
        if !self.store.active(job).await? {
            return Ok(());
        }
        let url = Url::parse(&job.url).context("Stored portal URL is invalid")?;
        let http = self.fetcher.portal(&url);
        let ctx = ReasoningContext {
            job,
            url: &url,
            http: &http,
        };
        let previous = job
            .recipe
            .as_ref()
            .map(|data| serde_json::from_str::<Recipe>(data))
            .transpose()?;
        let baseline = self
            .store
            .tenders(&job.portal_id, None)
            .await?
            .unwrap_or_default();
        let prior = self.store.evidence(job).await?;
        let snapshot = Snapshot {
            tenders: &baseline,
            evidence: prior.as_ref(),
        };

        let Some(recipe) = previous else {
            return self.attach(&ctx, &snapshot).await;
        };
        let result = match self.read(&ctx, &job.id, &recipe).await {
            Ok(candidate) => self.check(&ctx, &recipe, candidate, &snapshot, false).await,
            Err(failure) => Err((Stage::Read, failure)),
        };
        let result = result.map_err(|(stage, failure)| {
            log_failed_recipe(&job.portal_id, &job.id, &recipe, stage, &failure);
            failure
        });
        match result {
            Ok(candidate) => {
                self.publish(job, &job.id, &recipe, candidate, "Read verified")
                    .await
            }
            Err(failure) if failure.is_temporary() => self.defer(job, &job.id, &failure).await,
            Err(failure @ ReadFailure::Blocked(_)) => {
                self.store
                    .fail(job, &job.id, &failure.to_string(), self.recovery.recheck)
                    .await
            }
            Err(failure) => {
                if let Some(until) = self.repair_cooldown(job).await? {
                    let at = chrono::DateTime::from_timestamp(until, 0)
                        .map(|time| time.to_rfc3339())
                        .unwrap_or_default();
                    return self
                        .store
                        .fail(
                            job,
                            &job.id,
                            &format!(
                                "Read still fails: {failure}. The last repair did not restore delivery; the next repair starts after {at}"
                            ),
                            self.interval,
                        )
                        .await;
                }
                self.repair(&ctx, recipe, failure, &snapshot).await
            }
        }
    }

    /// Reads the portal with `recipe`, retrying once after a short wait on a temporary failure.
    async fn read(
        &self,
        ctx: &ReasoningContext<'_>,
        run_id: &str,
        recipe: &Recipe,
    ) -> Result<CandidateRead, ReadFailure> {
        let read = recipe::read(ctx.http, ctx.url, recipe).await;
        if let Err(failure @ ReadFailure::Temporary(_)) = &read {
            tracing::warn!(portal_id = %ctx.job.portal_id, run_id, %failure, "Retrying portal read");
            tokio::time::sleep(Duration::from_secs(2)).await;
            return recipe::read(ctx.http, ctx.url, recipe).await;
        }
        read
    }

    /// The first attach of a portal without a recipe: choose a connection method, then learn,
    /// read and check it. If the route fails verification or yields an unexplained empty
    /// listing, try the remaining usable routes, each in a read run of its own, up to
    /// `MAX_ATTACH_METHODS`. Learning again costs model calls, so a failed attach waits for
    /// the recheck. A login or CAPTCHA fails the attach at once.
    async fn attach(&self, ctx: &ReasoningContext<'_>, snapshot: &Snapshot<'_>) -> Result<()> {
        let job = ctx.job;
        let decision = match self.connect(ctx).await? {
            Ok(decision) => decision,
            Err(failure) if failure.is_temporary() => {
                return self.defer(job, &job.id, &failure).await;
            }
            Err(failure) => {
                return self
                    .store
                    .fail(job, &job.id, &failure.to_string(), self.recovery.recheck)
                    .await;
            }
        };
        let mut run_id = job.id.clone();
        let outcome = self
            .through_methods(
                ctx,
                &mut run_id,
                decision,
                None,
                None,
                snapshot,
                Mode::Attach,
            )
            .await?;
        match outcome {
            MethodsOutcome::Published | MethodsOutcome::Removed => Ok(()),
            MethodsOutcome::Blocked(failure) => {
                self.store
                    .fail(job, &run_id, &failure.to_string(), self.recovery.recheck)
                    .await
            }
            MethodsOutcome::Failed { failure, .. } if failure.is_temporary() => {
                self.defer(job, &run_id, &failure).await
            }
            MethodsOutcome::Failed { tried, .. } => {
                self.store
                    .fail(job, &run_id, &attach_reason(&tried), self.recovery.recheck)
                    .await
            }
        }
    }

    /// When a repair ran out of attempts within the cooldown, returns the time the cooldown
    /// ends. A manual refresh skips the cooldown.
    async fn repair_cooldown(&self, job: &Job) -> Result<Option<i64>> {
        let retry = self.store.retry_state(job).await?;
        if retry.manual_run.as_deref() == Some(job.id.as_str()) {
            return Ok(None);
        }
        Ok(retry
            .last_repair_at
            .map(|at| at.saturating_add(self.recovery.repair_cooldown))
            .filter(|until| *until > chrono::Utc::now().timestamp()))
    }

    async fn defer(&self, job: &Job, run_id: &str, failure: &ReadFailure) -> Result<()> {
        let recovery = &self.recovery;
        let wait = failure
            .retry_after()
            .map(|wait| (wait.as_secs() as i64).min(recovery.retry_after_cap));
        let interval = self.interval;
        tracing::warn!(portal_id = %job.portal_id, run_id, %failure, "Read deferred after a temporary failure");
        self.store
            .defer(
                job,
                run_id,
                &failure.to_string(),
                |failures| {
                    let backoff = usize::try_from(failures - 1)
                        .ok()
                        .and_then(|index| recovery.backoff.get(index).copied())
                        .unwrap_or(interval);
                    wait.map_or(backoff, |wait| wait.max(backoff))
                },
                OutageLimit {
                    failures: recovery.outage_failures,
                    seconds: recovery.outage_seconds,
                },
            )
            .await
    }

    /// Applies the publication rule to a candidate read. An incomplete read is rejected
    /// outright. A complete read publishes without a model call unless `assess` raised a
    /// signal or `always_audit` is set, in which case the model audits it with the review.
    async fn check(
        &self,
        ctx: &ReasoningContext<'_>,
        recipe: &Recipe,
        candidate: CandidateRead,
        snapshot: &Snapshot<'_>,
        always_audit: bool,
    ) -> Result<CandidateRead, (Stage, ReadFailure)> {
        let review = assess(snapshot, &candidate).map_err(|error| (Stage::Check, error))?;
        if !always_audit && review.signals.is_empty() {
            return Ok(candidate);
        }
        tracing::info!(
            portal_id = %ctx.job.portal_id,
            signals = ?review.signals,
            "Read needs a model audit"
        );
        self.model
            .verify(ctx, recipe, &candidate, &review)
            .await
            .map(|()| candidate)
            .map_err(|error| (Stage::Verify, error))
    }

    /// Chooses how to read a portal without a recipe: probe for candidate methods, rank them,
    /// let the model decide, and store the decision as attempt 1. Earlier decisions for this
    /// portal generation are replaced, because a new attach probes afresh.
    async fn connect(
        &self,
        ctx: &ReasoningContext<'_>,
    ) -> Result<Result<ConnectionDecision, ReadFailure>> {
        let candidates = match discovery::probe(ctx.url, ctx.http).await {
            Ok(candidates) if candidates.is_empty() => {
                return Ok(Err(ReadFailure::Incomplete(
                    "Probing found no usable way to read this portal".into(),
                )));
            }
            Ok(candidates) => candidates,
            Err(failure) => return Ok(Err(failure)),
        };
        let ranked = discovery::rank_for(ctx.url, candidates);
        let mut decision = match self.model.decide(ctx, ranked).await {
            Ok(decision) => decision,
            Err(failure) => return Ok(Err(failure)),
        };
        decision.attempt = 1;
        self.store.clear_decisions(ctx.job).await?;
        self.store.record_decision(ctx.job, &decision).await?;
        tracing::info!(
            portal_id = %ctx.job.portal_id,
            method = decision.chosen.method.label(),
            entry_url = %decision.chosen.entry_url,
            rationale = %decision.rationale,
            "Connection method chosen"
        );
        Ok(Ok(decision))
    }

    /// A recipe for the decided method. The OCDS preset maps fields directly and needs no model
    /// call; every other method is learned by the model, starting from the chosen entry URL.
    async fn learn(
        &self,
        ctx: &ReasoningContext<'_>,
        previous: Option<(&Recipe, &str)>,
        decision: &ConnectionDecision,
    ) -> Result<Recipe, ReadFailure> {
        if let Some(recipe) = Recipe::preset(&decision.chosen) {
            return Ok(recipe);
        }
        let mut recipe = self.model.learn(ctx, previous, decision).await?;
        // A recipe without a start URL would read the submitted address, not the chosen entry.
        if recipe.start_url.is_none() && decision.chosen.entry_url != ctx.url.as_str() {
            recipe.start_url = Some(decision.chosen.entry_url.clone());
        }
        Ok(recipe)
    }

    async fn repair(
        &self,
        ctx: &ReasoningContext<'_>,
        recipe: Recipe,
        failure: ReadFailure,
        snapshot: &Snapshot<'_>,
    ) -> Result<()> {
        let job = ctx.job;
        let broke = describe_break(&failure, snapshot);
        let Some(mut run_id) = self
            .store
            .begin_repair(job, &failure.to_string(), &broke)
            .await?
        else {
            return Ok(());
        };
        let (selection, previous) = match self.store.latest_decision(&job.portal_id).await? {
            Some(saved) if saved.chosen.is_usable() => (Ok(saved), Some(recipe)),
            Some(saved) => (self.replace_unusable_choice(ctx, saved).await?, None),
            None => (self.connect(ctx).await?, Some(recipe)),
        };
        let decision = match selection {
            Ok(decision) => decision,
            Err(error) if error.is_temporary() => return self.defer(job, &run_id, &error).await,
            Err(error) => {
                return self
                    .store
                    .fail(job, &run_id, &error.to_string(), self.recovery.recheck)
                    .await;
            }
        };
        let outcome = self
            .through_methods(
                ctx,
                &mut run_id,
                decision,
                previous,
                Some(failure),
                snapshot,
                Mode::Repair(&broke),
            )
            .await?;
        let (tried, failure) = match outcome {
            MethodsOutcome::Published | MethodsOutcome::Removed => return Ok(()),
            MethodsOutcome::Blocked(error) => {
                return self
                    .store
                    .fail(job, &run_id, &error.to_string(), self.recovery.recheck)
                    .await;
            }
            MethodsOutcome::Failed { tried, failure } => (tried, failure),
        };
        if failure.is_temporary() {
            return self.defer(job, &run_id, &failure).await;
        }
        self.store.record_repair(job).await?;
        // With a stored recipe the next read is free. Without one the portal never attached,
        // and every read would pay to learn again.
        let retry_in = if job.recipe.is_some() {
            self.interval
        } else {
            self.recovery.recheck
        };
        let reason = if tried.len() > 1 {
            let names: Vec<_> = tried.iter().map(|tried| tried.method.label()).collect();
            format!("{} both failed: {failure}", names.join(" and "))
        } else {
            format!("Repair did not restore delivery: {failure}")
        };
        self.store.fail(job, &run_id, &reason, retry_in).await
    }

    /// Older decisions can name a method whose reader is no longer available.
    async fn replace_unusable_choice(
        &self,
        ctx: &ReasoningContext<'_>,
        saved: ConnectionDecision,
    ) -> Result<Result<ConnectionDecision, ReadFailure>> {
        let mut candidates = saved.alternatives;
        candidates.push(saved.chosen);
        let (remaining, evidence): (Vec<_>, Vec<_>) = discovery::rank_for(ctx.url, candidates)
            .into_iter()
            .partition(|candidate| candidate.is_usable());
        if remaining.is_empty() {
            return self.connect(ctx).await;
        }
        let mut next = match self.model.redecide(ctx, remaining).await {
            Ok(next) => next,
            Err(error) => return Ok(Err(error)),
        };
        next.alternatives.extend(evidence);
        next.attempt = saved.attempt + 1;
        self.store.record_decision(ctx.job, &next).await?;
        Ok(Ok(next))
    }

    /// Tries the decided connection method, then the next usable stored alternatives in ranked
    /// order, until one publishes, a login or CAPTCHA blocks delivery, a temporary failure
    /// interrupts, or `mode` allows no further method. Each new method is a recorded decision;
    /// on attach it also gets a read run of its own, recorded in `run_id`.
    #[allow(clippy::too_many_arguments)]
    async fn through_methods(
        &self,
        ctx: &ReasoningContext<'_>,
        run_id: &mut String,
        mut decision: ConnectionDecision,
        mut recipe: Option<Recipe>,
        mut failure: Option<ReadFailure>,
        snapshot: &Snapshot<'_>,
        mode: Mode<'_>,
    ) -> Result<MethodsOutcome> {
        let job = ctx.job;
        let mut tried: Vec<TriedMethod> = Vec::new();
        loop {
            let run = MethodRun {
                id: run_id,
                mode,
                tried: &tried,
            };
            let last = match self
                .try_method(ctx, &run, &mut recipe, &mut failure, snapshot, &decision)
                .await?
            {
                MethodOutcome::Published => return Ok(MethodsOutcome::Published),
                MethodOutcome::Blocked(error) => return Ok(MethodsOutcome::Blocked(error)),
                MethodOutcome::Exhausted(error) => error,
            };
            tried.push(TriedMethod {
                method: decision.chosen.method,
                entry_url: decision.chosen.entry_url.clone(),
                failure: last.to_string(),
            });
            if last.is_temporary() || tried.len() >= mode.max_methods() {
                return Ok(MethodsOutcome::Failed {
                    tried,
                    failure: last,
                });
            }
            let remaining: Vec<_> = decision
                .alternatives
                .iter()
                .filter(|candidate| {
                    candidate.is_usable()
                        && !tried.iter().any(|t| {
                            t.method == candidate.method && t.entry_url == candidate.entry_url
                        })
                })
                .cloned()
                .collect();
            if remaining.is_empty() {
                return Ok(MethodsOutcome::Failed {
                    tried,
                    failure: last,
                });
            }
            let mut next = match self.model.redecide(ctx, remaining).await {
                Ok(next) => next,
                Err(error) => {
                    return Ok(MethodsOutcome::Failed {
                        tried,
                        failure: error,
                    });
                }
            };
            // Unusable candidates, such as TED, stay in the recorded evidence.
            next.alternatives.extend(
                decision
                    .alternatives
                    .iter()
                    .filter(|candidate| !candidate.is_usable())
                    .cloned(),
            );
            next.attempt = decision.attempt + 1;
            next.rationale = format!(
                "{} failed during {} ({}). {}",
                decision.chosen.method.label(),
                mode.as_str(),
                recipe::sample(&last.to_string(), 300),
                next.rationale
            );
            self.store.record_decision(job, &next).await?;
            tracing::warn!(
                portal_id = %job.portal_id,
                method = next.chosen.method.label(),
                attempt = next.attempt,
                mode = mode.as_str(),
                "Falling back to the next connection method"
            );
            if mode.is_attach() {
                let reason = format!(
                    "{} failed: {}. Trying {}",
                    decision.chosen.method.label(),
                    recipe::sample(&last.to_string(), 300),
                    next.chosen.method.label()
                );
                match self.store.begin_fallback(job, run_id, &reason).await? {
                    Some(id) => *run_id = id,
                    None => return Ok(MethodsOutcome::Removed),
                }
                // A recipe for another method is no starting point for this one.
                recipe = None;
                failure = None;
            } else {
                failure = Some(last);
            }
            decision = next;
        }
    }

    /// Learns and checks recipes for one connection method: twice, or once for a preset that
    /// would produce the same recipe again. Publishes on success. Learning starts from
    /// `recipe` and `failure` when both are known. Empty first reads require source evidence.
    async fn try_method(
        &self,
        ctx: &ReasoningContext<'_>,
        run: &MethodRun<'_>,
        recipe: &mut Option<Recipe>,
        failure: &mut Option<ReadFailure>,
        snapshot: &Snapshot<'_>,
        decision: &ConnectionDecision,
    ) -> Result<MethodOutcome> {
        let job = ctx.job;
        let run_id = run.id.as_str();
        let attempts = if Recipe::preset(&decision.chosen).is_some() {
            1
        } else {
            2
        };
        for _ in 0..attempts {
            let reason = failure.as_ref().map(ToString::to_string);
            let previous = recipe.as_ref().zip(reason.as_deref());
            let learned = match self.learn(ctx, previous, decision).await {
                Ok(learned) => recipe.insert(learned),
                Err(error @ ReadFailure::Blocked(_)) => return Ok(MethodOutcome::Blocked(error)),
                Err(error) if error.is_temporary() => return Ok(MethodOutcome::Exhausted(error)),
                Err(error) => {
                    *failure = Some(error);
                    continue;
                }
            };
            let attempt = match self.read(ctx, run_id, learned).await {
                Ok(candidate)
                    if run.mode.is_attach()
                        && candidate.tenders.is_empty()
                        && !candidate.evidence.empty_state =>
                {
                    Err((
                        Stage::Read,
                        ReadFailure::Incomplete(format!(
                            "{} read found no tenders",
                            decision.chosen.method.label()
                        )),
                    ))
                }
                Ok(candidate) => self.check(ctx, learned, candidate, snapshot, true).await,
                Err(error) => Err((Stage::Read, error)),
            };
            let error = match attempt {
                Ok(candidate) => {
                    let note = run.note(job.recipe.as_deref(), learned, decision);
                    self.publish(job, run_id, learned, candidate, &note).await?;
                    return Ok(MethodOutcome::Published);
                }
                Err((stage, error)) => {
                    log_failed_recipe(&job.portal_id, run_id, learned, stage, &error);
                    error
                }
            };
            match error {
                ReadFailure::Blocked(_) => return Ok(MethodOutcome::Blocked(error)),
                error if error.is_temporary() => return Ok(MethodOutcome::Exhausted(error)),
                error => *failure = Some(error),
            }
        }
        Ok(MethodOutcome::Exhausted(failure.take().unwrap_or_else(
            || {
                ReadFailure::Incomplete(format!(
                    "{} could not be learned",
                    decision.chosen.method.label()
                ))
            },
        )))
    }

    async fn publish(
        &self,
        job: &Job,
        run_id: &str,
        recipe: &Recipe,
        candidate: CandidateRead,
        note: &str,
    ) -> Result<()> {
        self.store
            .publish(
                job,
                run_id,
                &serde_json::to_string(recipe)?,
                candidate,
                self.interval,
                note,
            )
            .await
    }
}

/// The run reason of a read that broke: the failure and what was being served before it.
fn describe_break(failure: &ReadFailure, snapshot: &Snapshot<'_>) -> String {
    let held = snapshot.tenders.len();
    if held == 0 {
        return format!("{failure}. No verified read to fall back on yet");
    }
    let open = snapshot
        .tenders
        .iter()
        .filter(|published| published.tender.status == TenderStatus::Open)
        .count();
    format!(
        "{failure}. The last verified read held {held} tender(s), {open} open, and stays served"
    )
}

/// Recipe settings named in a repair reason before the rest are only counted.
const NOTE_CHANGES: usize = 4;

/// The run reason of a repair that published: the method, what changed in the recipe, and the
/// failure it repaired.
fn repair_note(
    previous: Option<&str>,
    recipe: &Recipe,
    decision: &ConnectionDecision,
    broke: &str,
) -> String {
    let method = decision.chosen.method.label();
    let previous = previous.and_then(|data| serde_json::from_str::<Value>(data).ok());
    let changes = match previous {
        None => "learned a new recipe".to_owned(),
        Some(previous) => {
            let mut changes = Vec::new();
            recipe_changes(
                "",
                &previous,
                &serde_json::to_value(recipe).unwrap_or_default(),
                &mut changes,
            );
            match changes.len() {
                0 => "kept the recipe, which verified on a fresh read".to_owned(),
                count if count <= NOTE_CHANGES => format!("changed {}", changes.join("; ")),
                count => format!(
                    "changed {}; and {} more setting(s)",
                    changes[..NOTE_CHANGES].join("; "),
                    count - NOTE_CHANGES
                ),
            }
        }
    };
    format!(
        "Repaired via {method}: {changes}. Fixes: {}",
        recipe::sample(broke, 300)
    )
}

/// Collects `path: before → after` for every recipe setting that differs, recursing into
/// objects so a changed selector names its field.
fn recipe_changes(path: &str, before: &Value, after: &Value, changes: &mut Vec<String>) {
    if before == after {
        return;
    }
    if let (Value::Object(before), Value::Object(after)) = (before, after) {
        let keys: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
        for key in keys {
            let path = if path.is_empty() {
                key.clone()
            } else {
                format!("{path}.{key}")
            };
            recipe_changes(
                &path,
                before.get(key).unwrap_or(&Value::Null),
                after.get(key).unwrap_or(&Value::Null),
                changes,
            );
        }
        return;
    }
    let show = |value: &Value| match value {
        Value::Null => "unset".to_owned(),
        Value::String(text) => format!("\"{}\"", recipe::sample(text, 60)),
        other => recipe::sample(&other.to_string(), 60),
    };
    changes.push(format!("{path} {} → {}", show(before), show(after)));
}

/// Distinct connection routes a repair tries before the portal is failed.
const MAX_METHODS: usize = 2;
/// Distinct connection routes a first attach tries before the portal is failed.
const MAX_ATTACH_METHODS: usize = 3;

/// Why methods are being tried: a first attach without a recipe, or a repair of one. A
/// repair carries the run reason of the read that broke.
#[derive(Clone, Copy, Debug)]
enum Mode<'a> {
    Attach,
    Repair(&'a str),
}

impl Mode<'_> {
    fn is_attach(self) -> bool {
        matches!(self, Self::Attach)
    }

    fn max_methods(self) -> usize {
        match self {
            Self::Attach => MAX_ATTACH_METHODS,
            Self::Repair(_) => MAX_METHODS,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Attach => "attach",
            Self::Repair(_) => "repair",
        }
    }
}

/// The run a connection method is tried in, and the methods that failed before it.
struct MethodRun<'a> {
    id: &'a String,
    mode: Mode<'a>,
    tried: &'a [TriedMethod],
}

impl MethodRun<'_> {
    /// The run reason when this method publishes.
    fn note(
        &self,
        previous: Option<&str>,
        recipe: &Recipe,
        decision: &ConnectionDecision,
    ) -> String {
        match self.mode {
            Mode::Repair(broke) => repair_note(previous, recipe, decision, broke),
            Mode::Attach if self.tried.is_empty() => "Attached and verified".to_owned(),
            Mode::Attach => {
                let failed: Vec<_> = self
                    .tried
                    .iter()
                    .map(|tried| tried.method.label())
                    .collect();
                format!(
                    "Attached and verified via {} after {} failed",
                    decision.chosen.method.label(),
                    failed.join(" and ")
                )
            }
        }
    }
}

enum MethodOutcome {
    Published,
    Blocked(ReadFailure),
    /// Every attempt on this method failed, or a temporary failure cut it short.
    Exhausted(ReadFailure),
}

enum MethodsOutcome {
    Published,
    Blocked(ReadFailure),
    /// The portal was removed while methods were tried.
    Removed,
    /// No method delivered. `failure` is the last one, which decides between deferring and
    /// failing.
    Failed {
        tried: Vec<TriedMethod>,
        failure: ReadFailure,
    },
}

/// A connection method that did not deliver, with its last failure.
struct TriedMethod {
    method: ConnectionMethod,
    entry_url: String,
    failure: String,
}

/// The `failed` reason of an attach, naming every method tried with its last failure.
fn attach_reason(tried: &[TriedMethod]) -> String {
    let details: Vec<_> = tried
        .iter()
        .map(|tried| {
            format!(
                "{}: {}",
                tried.method.label(),
                recipe::sample(&tried.failure, 300)
            )
        })
        .collect();
    match tried {
        [] => "Attach tried no connection method".into(),
        [only] => format!("{} failed: {}", only.method.label(), only.failure),
        _ => {
            let names: Vec<_> = tried.iter().map(|tried| tried.method.label()).collect();
            let (last, rest) = names.split_last().expect("several methods");
            format!(
                "Attach tried {} and {last}; none delivered verified tenders. {}",
                rest.join(", "),
                details.join("; ")
            )
        }
    }
}

/// Where a recipe attempt failed: while reading the portal, when the read was compared with
/// the published snapshot, or when the model audited the candidate read.
#[derive(Clone, Copy, Debug)]
enum Stage {
    Read,
    Check,
    Verify,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Check => "check",
            Self::Verify => "verify",
        }
    }
}

/// The published state a candidate read is compared with.
struct Snapshot<'a> {
    tenders: &'a [PublishedTender],
    /// Listing evidence of the last verified read. `None` before the first one, and for
    /// portals published before this evidence was stored.
    evidence: Option<&'a StoredEvidence>,
}

const REVIEW_LIMIT: usize = 20;

/// Fields compared between a published tender and its new read. A change to one of them on at
/// least two tenders, and on at least a quarter of the open ones, in a single read is treated
/// as systematic and audited.
const COMPARED_FIELDS: &[&str] = &[
    "url",
    "title",
    "buyerName",
    "publicationDate",
    "submissionDeadline",
    "status",
    "documents",
    "referenceNumber",
    "description",
    "procedureType",
    "contractNature",
    "cpvCodes",
    "placeOfPerformance",
];

/// Applies the publication rule's comparison with the published snapshot.
///
/// Returns an error when the read is incomplete: it has no positive end-of-list evidence,
/// it skipped a listing page that the last verified read fetched and that is still linked, or
/// more than half of the open tenders (at least two) vanished while the listing shows no empty
/// state. Otherwise returns a review. Its signals are empty only when nothing suggests the read
/// is wrong, in which case a repeat read publishes without a model call.
fn assess(snapshot: &Snapshot<'_>, candidate: &CandidateRead) -> Result<Review, ReadFailure> {
    let evidence = &candidate.evidence;
    if !evidence.terminal_page {
        return Err(ReadFailure::Incomplete(
            "Read found no evidence that the listing ended".into(),
        ));
    }
    let mut signals = Vec::new();
    match snapshot.evidence {
        Some(prior) => {
            if let Some(page) = prior.listing_pages.iter().find(|page| {
                !evidence.listing_pages.contains(page) && evidence.listing_links.contains(*page)
            }) {
                return Err(ReadFailure::Incomplete(format!(
                    "Listing page {} from the last verified read is still linked but was not read",
                    location(page)
                )));
            }
            if prior.listing_pages.len() != evidence.listing_pages.len() {
                signals.push(format!(
                    "The listing spans {} page(s), down or up from {}",
                    evidence.listing_pages.len(),
                    prior.listing_pages.len()
                ));
            }
            let new_links = evidence
                .unfollowed_links
                .iter()
                .filter(|link| !prior.unfollowed_links.contains(link))
                .count();
            if new_links > 0 {
                signals.push(format!(
                    "{new_links} new pagination link(s) on the listing were not followed"
                ));
            }
        }
        None => signals.push("No verified listing evidence to compare with".into()),
    }

    let candidates: HashMap<String, &Tender> = candidate
        .tenders
        .iter()
        .filter_map(|tender| Some((tender.key()?, tender)))
        .collect();
    let open: Vec<&Tender> = snapshot
        .tenders
        .iter()
        .map(|published| &published.tender)
        .filter(|tender| tender.status == TenderStatus::Open)
        .collect();
    let vanished: Vec<&Tender> = open
        .iter()
        .copied()
        .filter(|tender| {
            tender
                .key()
                .is_none_or(|key| !candidates.contains_key(&key))
        })
        .collect();
    if vanished.len() >= 2 && vanished.len() * 2 > open.len() && !evidence.empty_state {
        return Err(ReadFailure::Incomplete(format!(
            "{} of {} open tenders vanished while the listing shows no empty state",
            vanished.len(),
            open.len()
        )));
    }
    if !vanished.is_empty() {
        signals.push(format!(
            "{} previously open tender(s) are no longer listed",
            vanished.len()
        ));
    }

    let mut changes = Vec::new();
    let mut changed_counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut lost = 0;
    for old in &open {
        let Some(new) = old.key().and_then(|key| candidates.get(&key).copied()) else {
            continue;
        };
        if new.documents.len() < old.documents.len()
            || (old.title.is_some() && new.title.is_none())
            || (old.buyer_name.is_some() && new.buyer_name.is_none())
            || (old.submission_deadline.is_some() && new.submission_deadline.is_none())
        {
            lost += 1;
        }
        let (before, after) = (comparable(old), comparable(new));
        for field in COMPARED_FIELDS {
            let (before, after) = (
                before.get(*field).cloned().unwrap_or(Value::Null),
                after.get(*field).cloned().unwrap_or(Value::Null),
            );
            if before != after {
                *changed_counts.entry(field).or_default() += 1;
                if changes.len() < REVIEW_LIMIT {
                    changes.push(FieldChange {
                        tender: label(old),
                        field,
                        before,
                        after,
                    });
                }
            }
        }
    }
    if lost > 0 {
        signals.push(format!(
            "{lost} tender(s) lost documents, a title, a buyer or a deadline"
        ));
    }
    for (field, count) in &changed_counts {
        if *count >= 2 && count * 4 >= open.len() {
            signals.push(format!(
                "{field} changed on {count} of {} open tenders",
                open.len()
            ));
        }
    }

    let mut owners: HashMap<&str, Vec<String>> = HashMap::new();
    for published in snapshot.tenders {
        if let Some(key) = published.tender.key() {
            for document in &published.tender.documents {
                owners
                    .entry(document.sha256.as_str())
                    .or_default()
                    .push(key.clone());
            }
        }
    }
    let known: HashSet<String> = snapshot
        .tenders
        .iter()
        .filter_map(|published| published.tender.key())
        .collect();
    let mut moved_files = Vec::new();
    for (key, tender) in &candidates {
        if !known.contains(key) {
            continue;
        }
        for document in &tender.documents {
            if let Some(from) = owners.get(document.sha256.as_str())
                && !from.contains(key)
            {
                moved_files.push(MovedFile {
                    file_name: document.file_name.clone(),
                    sha256: document.sha256.clone(),
                    from: from.clone(),
                    to: key.clone(),
                });
            }
        }
    }
    if !moved_files.is_empty() {
        signals.push(format!(
            "{} file(s) moved from one tender to another",
            moved_files.len()
        ));
        moved_files.truncate(REVIEW_LIMIT);
    }

    Ok(Review {
        previous_open_count: open.len(),
        signals,
        vanished: vanished
            .iter()
            .take(REVIEW_LIMIT)
            .map(|tender| TenderSummary {
                id: tender.id.clone(),
                url: tender.url.clone(),
                title: tender.title.clone(),
            })
            .collect(),
        changes,
        moved_files,
        unfollowed_links: evidence.unfollowed_links.clone(),
    })
}

/// A tender as compared field by field. Documents are reduced to their names and hashes.
fn comparable(tender: &Tender) -> serde_json::Map<String, Value> {
    let mut value = serde_json::to_value(tender).unwrap_or_default();
    let documents: Vec<Value> = tender
        .documents
        .iter()
        .map(|document| {
            Value::String(format!(
                "{} ({})",
                document.file_name,
                &document.sha256[..document.sha256.len().min(12)]
            ))
        })
        .collect();
    if let Some(map) = value.as_object_mut() {
        map.insert("documents".into(), Value::Array(documents));
        return std::mem::take(map);
    }
    serde_json::Map::new()
}

fn label(tender: &Tender) -> String {
    tender
        .id
        .clone()
        .or_else(|| tender.url.clone())
        .unwrap_or_default()
}

fn location(url: &str) -> String {
    match Url::parse(url) {
        Ok(url) => format!("{}{}", url.host_str().unwrap_or_default(), url.path()),
        Err(_) => url.chars().take(240).collect(),
    }
}

/// Records the recipe behind a failed attempt. Recipes are stored only when a read is
/// published, so this log line is the only record of what a failed learn or repair produced.
fn log_failed_recipe(
    portal_id: &str,
    run_id: &str,
    recipe: &Recipe,
    stage: Stage,
    failure: &ReadFailure,
) {
    let recipe = serde_json::to_string(recipe)
        .unwrap_or_else(|error| format!("<recipe not serializable: {error}>"));
    tracing::warn!(
        portal_id,
        run_id,
        stage = stage.as_str(),
        %failure,
        recipe,
        "Recipe attempt failed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_attempt_logs_run_stage_reason_and_recipe() {
        let recipe: Recipe = serde_json::from_value(serde_json::json!({
            "listing": "html", "items": ".gem-c-document-list__item",
            "documents": {"path": "h3.gem-c-attachment__title a", "attribute": "href"}
        }))
        .unwrap();
        let output = Captured::default();
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            log_failed_recipe(
                "portal-1",
                "run-1",
                &recipe,
                Stage::Verify,
                &ReadFailure::Incomplete("Inquest record has no documents".into()),
            );
        });
        let line = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        for expected in [
            "WARN",
            "Recipe attempt failed",
            "portal_id=\"portal-1\"",
            "run_id=\"run-1\"",
            "stage=\"verify\"",
            "failure=Inquest record has no documents",
            "h3.gem-c-attachment__title a",
            ".gem-c-document-list__item",
        ] {
            assert!(line.contains(expected), "missing {expected:?} in {line}");
        }
    }

    fn tender(id: &str, files: &[(&str, &str)]) -> Tender {
        serde_json::from_value(serde_json::json!({
            "id": id, "url": format!("https://portal.example/{id}"), "title": format!("Tender {id}"),
            "buyerName": "City", "publicationDate": null, "submissionDeadline": null, "status": "open",
            "documents": files.iter().map(|(name, sha)| serde_json::json!({
                "fileName": name, "sizeBytes": 1, "sha256": sha, "path": format!("/files/{sha}")
            })).collect::<Vec<_>>()
        }))
        .unwrap()
    }

    fn candidate(tenders: Vec<Tender>, pages: &[&str]) -> CandidateRead {
        CandidateRead {
            evidence: crate::domain::Evidence {
                pages: pages.len(),
                entries: tenders.len(),
                terminal_page: true,
                end_of_list: "last-page",
                total: None,
                empty_state: false,
                document_count: 0,
                skipped_documents: Vec::new(),
                listing_pages: pages.iter().map(|page| page.to_string()).collect(),
                unfollowed_links: Vec::new(),
                samples: Vec::new(),
                listing_links: HashSet::new(),
            },
            tenders,
        }
    }

    fn published(tenders: Vec<Tender>) -> Vec<PublishedTender> {
        tenders
            .into_iter()
            .map(|tender| PublishedTender {
                tender,
                last_changed_at: chrono::Utc::now(),
            })
            .collect()
    }

    const SHARED: &str = "0000000000000000000000000000000000000000000000000000000000000001";
    const OWN_A: &str = "00000000000000000000000000000000000000000000000000000000000000aa";
    const OWN_B: &str = "00000000000000000000000000000000000000000000000000000000000000bb";

    #[test]
    fn shared_boilerplate_is_not_a_moved_file_but_an_own_file_is() {
        let page = ["https://portal.example/"];
        let before = published(vec![
            tender("A", &[("terms.pdf", SHARED), ("a.pdf", OWN_A)]),
            tender("B", &[("terms.pdf", SHARED), ("b.pdf", OWN_B)]),
        ]);
        let prior = StoredEvidence {
            listing_pages: vec![page[0].into()],
            unfollowed_links: Vec::new(),
        };
        let snapshot = Snapshot {
            tenders: &before,
            evidence: Some(&prior),
        };
        let same = candidate(
            vec![
                tender("A", &[("terms.pdf", SHARED), ("a.pdf", OWN_A)]),
                tender("B", &[("terms.pdf", SHARED), ("b.pdf", OWN_B)]),
            ],
            &page,
        );
        let review = assess(&snapshot, &same).unwrap();
        assert!(review.signals.is_empty(), "{:?}", review.signals);

        let swapped = candidate(
            vec![
                tender("A", &[("terms.pdf", SHARED), ("b.pdf", OWN_B)]),
                tender("B", &[("terms.pdf", SHARED), ("a.pdf", OWN_A)]),
            ],
            &page,
        );
        let review = assess(&snapshot, &swapped).unwrap();
        assert_eq!(review.moved_files.len(), 2);
        assert!(review.signals.iter().any(|signal| signal.contains("moved")));
        assert!(
            review
                .signals
                .iter()
                .any(|signal| signal.contains("documents changed on 2 of 2"))
        );
    }

    #[test]
    fn one_amended_tender_among_many_needs_no_audit() {
        let page = ["https://portal.example/"];
        let ids = ["A", "B", "C", "D", "E", "F", "G", "H", "I"];
        let before = published(ids.iter().map(|id| tender(id, &[])).collect());
        let prior = StoredEvidence {
            listing_pages: vec![page[0].into()],
            unfollowed_links: Vec::new(),
        };
        let snapshot = Snapshot {
            tenders: &before,
            evidence: Some(&prior),
        };
        let mut tenders: Vec<_> = ids.iter().map(|id| tender(id, &[])).collect();
        tenders[0].title = Some("Tender A, amended".into());
        tenders[1].title = Some("Tender B, amended".into());
        let review = assess(&snapshot, &candidate(tenders, &page)).unwrap();
        assert_eq!(review.changes.len(), 2);
        assert!(
            review.signals.is_empty(),
            "two of nine is below a quarter: {:?}",
            review.signals
        );
    }
}
