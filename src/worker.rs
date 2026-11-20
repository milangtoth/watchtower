use crate::{
    db::{Job, Store},
    domain::{CandidateRead, ReadFailure, TenderStatus},
    fetch::Fetcher,
    model::{PortalModel, ReasoningContext},
    recipe::{self, Recipe},
};
use anyhow::{Context, Result};
use reqwest::Url;
use std::{collections::HashMap, sync::Arc, time::Duration};
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
    pub interval: i64,
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
            while running.len() < 4 && !stop.is_cancelled() {
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
                                if let Err(error) = worker.store.fail_job(&job, "Internal error while reading the portal").await {
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
        let prior_open = baseline
            .iter()
            .filter(|tender| tender.tender.status == TenderStatus::Open)
            .count() as i64;

        let recipe = match previous.as_ref() {
            Some(recipe) => Ok(recipe.clone()),
            None => self.model.learn(&ctx, None).await,
        };
        let recipe = match recipe {
            Ok(recipe) => recipe,
            Err(failure) => return self.store.fail(job, &job.id, &failure.to_string()).await,
        };
        let mut read = recipe::read(&http, &url, &recipe).await;
        if let Err(failure @ ReadFailure::Temporary(_)) = &read {
            tracing::warn!(portal_id = %job.portal_id, run_id = %job.id, %failure, "Retrying portal read");
            tokio::time::sleep(Duration::from_secs(2)).await;
            read = recipe::read(&http, &url, &recipe).await;
        }
        let stage = if read.is_ok() {
            Stage::Verify
        } else {
            Stage::Read
        };
        let result = match read {
            Ok(candidate) => {
                let suspicious = baseline
                    .iter()
                    .filter(|previous| previous.tender.status == TenderStatus::Open)
                    .any(|previous| {
                        let old = &previous.tender;
                        candidate
                            .tenders
                            .iter()
                            .find(|new| new.key() == old.key())
                            .is_none_or(|new| {
                                new.documents.len() < old.documents.len()
                                    || (old.title.is_some() && new.title.is_none())
                                    || (old.buyer_name.is_some() && new.buyer_name.is_none())
                                    || (old.submission_deadline.is_some()
                                        && new.submission_deadline.is_none())
                            })
                    });
                if previous.is_none() || suspicious {
                    self.model
                        .verify(&ctx, &recipe, &candidate, prior_open)
                        .await
                        .map(|()| candidate)
                } else {
                    Ok(candidate)
                }
            }
            Err(failure) => Err(failure),
        };
        if let Err(failure) = &result {
            log_failed_recipe(&job.portal_id, &job.id, &recipe, stage, failure);
        }
        match result {
            Ok(candidate) => self.publish(job, &job.id, &recipe, candidate).await,
            Err(failure @ (ReadFailure::Blocked(_) | ReadFailure::Temporary(_))) => {
                self.store.fail(job, &job.id, &failure.to_string()).await
            }
            Err(failure) => self.repair(&ctx, recipe, failure, prior_open).await,
        }
    }

    async fn repair(
        &self,
        ctx: &ReasoningContext<'_>,
        mut recipe: Recipe,
        mut failure: ReadFailure,
        prior_open: i64,
    ) -> Result<()> {
        let job = ctx.job;
        let Some(run_id) = self.store.begin_repair(job, &failure.to_string()).await? else {
            return Ok(());
        };
        for _ in 0..2 {
            let reason = failure.to_string();
            match self.model.learn(ctx, Some((&recipe, &reason))).await {
                Ok(repaired) => recipe = repaired,
                Err(error @ ReadFailure::Blocked(_)) => {
                    return self.store.fail(job, &run_id, &error.to_string()).await;
                }
                Err(error) => {
                    failure = error;
                    continue;
                }
            }
            let attempt = match recipe::read(ctx.http, ctx.url, &recipe).await {
                Ok(candidate) => self
                    .model
                    .verify(ctx, &recipe, &candidate, prior_open)
                    .await
                    .map(|()| candidate)
                    .map_err(|error| (Stage::Verify, error)),
                Err(error) => Err((Stage::Read, error)),
            };
            let error = match attempt {
                Ok(candidate) => return self.publish(job, &run_id, &recipe, candidate).await,
                Err((stage, error)) => {
                    log_failed_recipe(&job.portal_id, &run_id, &recipe, stage, &error);
                    error
                }
            };
            if let ReadFailure::Blocked(_) = error {
                return self.store.fail(job, &run_id, &error.to_string()).await;
            }
            failure = error;
        }
        self.store
            .fail(
                job,
                &run_id,
                &format!("Repair did not restore delivery: {failure}"),
            )
            .await
    }

    async fn publish(
        &self,
        job: &Job,
        run_id: &str,
        recipe: &Recipe,
        candidate: CandidateRead,
    ) -> Result<()> {
        self.store
            .publish(
                job,
                run_id,
                &serde_json::to_string(recipe)?,
                candidate,
                self.interval,
            )
            .await
    }
}

/// Where a recipe attempt failed: while reading the portal, or when the model checked the
/// candidate read.
#[derive(Clone, Copy, Debug)]
enum Stage {
    Read,
    Verify,
}

impl Stage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Verify => "verify",
        }
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
}
