use crate::domain::{
    CandidateRead, ConnectionDecision, ModelOperation, ModelUsage, Portal, PublishedTender, Run,
    RunHistory, StoredEvidence, TenderStatus,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use sqlx::{
    FromRow, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{collections::BTreeMap, path::Path, time::Duration};
use uuid::Uuid;

#[derive(Clone)]
pub struct Store(SqlitePool);

#[derive(Clone, Debug, FromRow)]
pub struct Job {
    pub id: String,
    pub portal_id: String,
    pub generation: i64,
    pub url: String,
    pub recipe: Option<String>,
    pub last_success_at: Option<String>,
}

/// Retry bookkeeping for one portal generation.
#[derive(Clone, Debug, Default, FromRow)]
pub struct RetryState {
    /// Temporary failures since the last published read.
    pub temporary_failures: i64,
    /// When a repair last ran out of attempts, as a Unix timestamp.
    pub last_repair_at: Option<i64>,
    /// The run a person asked for with a manual refresh.
    pub manual_run: Option<String>,
}

/// When a run of temporary failures turns a portal failed.
#[derive(Clone, Copy, Debug)]
pub struct OutageLimit {
    pub failures: i64,
    pub seconds: i64,
}

/// True unless the `auto_refresh` preference was stored as off. A missing row means on.
const AUTO_REFRESH: &str =
    "COALESCE((SELECT enabled FROM preferences WHERE name = 'auto_refresh'), 1) = 1";

const PORTAL_COLUMNS: &str = "id, url, state, reason, tender_count, last_success_at";
const INSERT_RUN: &str =
    "INSERT INTO runs(id, portal_id, generation, kind) VALUES(?, ?, ?, 'read')";

impl Store {
    pub async fn open(path: &Path) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::raw_sql(include_str!("schema.sql"))
            .execute(&pool)
            .await?;
        migrate(&pool).await?;
        Ok(Self(pool))
    }

    pub async fn register(&self, url: &str) -> Result<String> {
        let mut tx = self.0.begin().await?;
        let existing: Option<(String, i64, bool)> =
            sqlx::query_as("SELECT id, generation, deleted FROM portals WHERE url = ?")
                .bind(url)
                .fetch_optional(&mut *tx)
                .await?;
        let (id, generation) = match existing {
            Some((id, _, false)) => return Ok(id),
            Some((id, generation, true)) => {
                let generation = generation + 1;
                sqlx::query("UPDATE portals SET deleted = 0, generation = ?, state = 'integrating', reason = 'Waiting for the first read', recipe = NULL, tender_count = 0, last_success_at = NULL, next_read_at = NULL WHERE id = ?")
                    .bind(generation).bind(&id).execute(&mut *tx).await?;
                (id, generation)
            }
            None => {
                let id = Uuid::new_v4().to_string();
                sqlx::query("INSERT INTO portals(id, url) VALUES(?, ?)")
                    .bind(&id)
                    .bind(url)
                    .execute(&mut *tx)
                    .await?;
                (id, 1)
            }
        };
        sqlx::query(INSERT_RUN)
            .bind(Uuid::new_v4().to_string())
            .bind(&id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn list(&self) -> Result<Vec<Portal>> {
        Ok(sqlx::query_as(&format!(
            "SELECT {PORTAL_COLUMNS} FROM portals WHERE deleted = 0 ORDER BY url"
        ))
        .fetch_all(&self.0)
        .await?)
    }

    pub async fn portal(&self, id: &str) -> Result<Option<Portal>> {
        Ok(sqlx::query_as(&format!(
            "SELECT {PORTAL_COLUMNS} FROM portals WHERE id = ? AND deleted = 0"
        ))
        .bind(id)
        .fetch_optional(&self.0)
        .await?)
    }

    pub async fn refresh(&self, id: &str) -> Result<Option<String>> {
        let mut tx = self.0.begin().await?;
        let generation: Option<i64> =
            sqlx::query_scalar("SELECT generation FROM portals WHERE id = ? AND deleted = 0")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(generation) = generation else {
            return Ok(None);
        };
        let existing: Option<String> = sqlx::query_scalar("SELECT id FROM runs WHERE portal_id = ? AND generation = ? AND kind = 'read' AND finished_at IS NULL")
            .bind(id).bind(generation).fetch_optional(&mut *tx).await?;
        let run_id = match existing {
            Some(run_id) => run_id,
            None => {
                let run_id = Uuid::new_v4().to_string();
                sqlx::query(INSERT_RUN)
                    .bind(&run_id)
                    .bind(id)
                    .bind(generation)
                    .execute(&mut *tx)
                    .await?;
                run_id
            }
        };
        // A manual refresh may start a repair even while the repair cooldown runs.
        sqlx::query("INSERT INTO portal_retry(portal_id, generation, manual_run) VALUES(?, ?, ?) ON CONFLICT(portal_id, generation) DO UPDATE SET manual_run = excluded.manual_run")
            .bind(id).bind(generation).bind(&run_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(run_id))
    }

    pub async fn remove(&self, id: &str) -> Result<Option<i64>> {
        let mut tx = self.0.begin().await?;
        let generation: Option<i64> = sqlx::query_scalar("UPDATE portals SET deleted = 1, next_read_at = NULL WHERE id = ? AND deleted = 0 RETURNING generation")
            .bind(id).fetch_optional(&mut *tx).await?;
        if let Some(generation) = generation {
            for table in [
                "runs",
                "tenders",
                "portal_files",
                "model_usage",
                "read_evidence",
                "portal_retry",
                "connection_decisions",
            ] {
                sqlx::query(&format!(
                    "DELETE FROM {table} WHERE portal_id = ? AND generation = ?"
                ))
                .bind(id)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(generation)
    }

    /// Prepares stored state after a restart. Interrupted work is recorded as failed and read
    /// again at once. A failed portal without a scheduled read, which older versions left
    /// behind, is rechecked after `recheck` seconds like any newly failed portal.
    pub async fn recover(&self, recheck: i64) -> Result<()> {
        let mut tx = self.0.begin().await?;
        sqlx::query("UPDATE portals SET next_read_at = ? WHERE deleted = 0 AND state = 'failed' AND next_read_at IS NULL")
            .bind(Utc::now().timestamp().saturating_add(recheck)).execute(&mut *tx).await?;
        sqlx::query("UPDATE portals SET next_read_at = 0 WHERE deleted = 0 AND (state IN ('integrating', 'healing') OR EXISTS (SELECT 1 FROM runs r WHERE r.portal_id = portals.id AND r.generation = portals.generation AND r.started_at IS NOT NULL AND r.finished_at IS NULL))")
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'failed', reason = COALESCE(reason, 'Interrupted by a service restart before the run finished') WHERE started_at IS NOT NULL AND finished_at IS NULL")
            .bind(Utc::now().to_rfc3339()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Whether scheduled reads run. On unless an operator turned it off: the service must
    /// read, notice and repair portals without anyone enabling it first.
    pub async fn auto_refresh(&self) -> Result<bool> {
        Ok(sqlx::query_scalar(&format!("SELECT {AUTO_REFRESH}"))
            .fetch_one(&self.0)
            .await?)
    }

    pub async fn set_auto_refresh(&self, enabled: bool) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO preferences(name, enabled) VALUES('auto_refresh', ?)")
            .bind(enabled)
            .execute(&self.0)
            .await?;
        Ok(())
    }

    pub async fn enqueue_due(&self) -> Result<()> {
        let mut tx = self.0.begin().await?;
        let portals: Vec<(String, i64)> = sqlx::query_as(&format!("SELECT id, generation FROM portals p WHERE {AUTO_REFRESH} AND deleted = 0 AND next_read_at <= ? AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.portal_id = p.id AND r.generation = p.generation AND r.finished_at IS NULL)"))
            .bind(Utc::now().timestamp()).fetch_all(&mut *tx).await?;
        for (id, generation) in portals {
            sqlx::query(INSERT_RUN)
                .bind(Uuid::new_v4().to_string())
                .bind(id)
                .bind(generation)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn claim(&self) -> Result<Option<Job>> {
        let mut tx = self.0.begin().await?;
        let job: Option<Job> = sqlx::query_as("SELECT r.id, r.portal_id, r.generation, p.url, p.recipe, p.last_success_at FROM runs r JOIN portals p ON p.id = r.portal_id AND p.generation = r.generation WHERE p.deleted = 0 AND r.started_at IS NULL AND NOT EXISTS (SELECT 1 FROM runs active WHERE active.portal_id = r.portal_id AND active.generation = r.generation AND active.started_at IS NOT NULL AND active.finished_at IS NULL) ORDER BY r.rowid LIMIT 1")
            .fetch_optional(&mut *tx).await?;
        if let Some(ref job) = job {
            // A failed attach is attaching again. A portal that served before stays failed
            // until a read publishes or a repair starts, because a plain read is no repair.
            if job.last_success_at.is_none() {
                sqlx::query("UPDATE portals SET state = 'integrating', reason = 'Retrying portal read' WHERE id = ? AND state = 'failed'")
                    .bind(&job.portal_id).execute(&mut *tx).await?;
            }
            sqlx::query("UPDATE runs SET started_at = ? WHERE id = ?")
                .bind(Utc::now().to_rfc3339())
                .bind(&job.id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(job)
    }

    pub async fn active(&self, job: &Job) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM portals WHERE id = ? AND generation = ? AND deleted = 0)",
        )
        .bind(&job.portal_id)
        .bind(job.generation)
        .fetch_one(&self.0)
        .await?)
    }

    /// Enters healing with `reason` as the portal reason, finishes the read that broke with
    /// `broke` as its run reason, and starts a repair run.
    pub async fn begin_repair(
        &self,
        job: &Job,
        reason: &str,
        broke: &str,
    ) -> Result<Option<String>> {
        let mut tx = self.0.begin().await?;
        let changed = sqlx::query("UPDATE portals SET state = 'healing', reason = ? WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(reason).bind(&job.portal_id).bind(job.generation).execute(&mut *tx).await?.rows_affected();
        if changed == 0 {
            return Ok(None);
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'failed', reason = ? WHERE id = ?")
            .bind(&now)
            .bind(broke)
            .bind(&job.id)
            .execute(&mut *tx)
            .await?;
        let id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO runs(id, portal_id, generation, kind, started_at) VALUES(?, ?, ?, 'repair', ?)")
            .bind(&id).bind(&job.portal_id).bind(job.generation).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(id))
    }

    /// Ends the attach run `run_id` as failed and starts a read run for the next connection
    /// method. The portal stays integrating, and `reason` becomes both the portal reason and
    /// the finished run's reason. `None` when the portal was removed.
    pub async fn begin_fallback(
        &self,
        job: &Job,
        run_id: &str,
        reason: &str,
    ) -> Result<Option<String>> {
        let mut tx = self.0.begin().await?;
        let changed = sqlx::query(
            "UPDATE portals SET reason = ? WHERE id = ? AND generation = ? AND deleted = 0",
        )
        .bind(reason)
        .bind(&job.portal_id)
        .bind(job.generation)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 0 {
            return Ok(None);
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'failed', reason = ? WHERE id = ?")
            .bind(&now)
            .bind(reason)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        let id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO runs(id, portal_id, generation, kind, started_at) VALUES(?, ?, ?, 'read', ?)")
            .bind(&id).bind(&job.portal_id).bind(job.generation).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(id))
    }

    /// Marks the portal failed and schedules its next read `retry_in` seconds from now. A failed
    /// portal keeps being read, so it recovers without a person once the portal does.
    pub async fn fail(&self, job: &Job, run_id: &str, reason: &str, retry_in: i64) -> Result<()> {
        let mut tx = self.0.begin().await?;
        sqlx::query("UPDATE portals SET state = 'failed', reason = ?, next_read_at = ? WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(reason).bind(Utc::now().timestamp().saturating_add(retry_in))
            .bind(&job.portal_id).bind(job.generation).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'failed', reason = ? WHERE id = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(reason)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn fail_job(&self, job: &Job, reason: &str, retry_in: i64) -> Result<()> {
        let run: Option<String> = sqlx::query_scalar("SELECT id FROM runs WHERE portal_id = ? AND generation = ? AND started_at IS NOT NULL AND finished_at IS NULL")
            .bind(&job.portal_id).bind(job.generation).fetch_optional(&self.0).await?;
        if let Some(run) = run {
            self.fail(job, &run, reason, retry_in).await?;
        }
        Ok(())
    }

    pub async fn publish(
        &self,
        job: &Job,
        run_id: &str,
        recipe: &str,
        candidate: CandidateRead,
        interval: i64,
        note: &str,
    ) -> Result<()> {
        anyhow::ensure!(
            candidate.evidence.terminal_page,
            "cannot publish incomplete pagination"
        );
        let evidence = serde_json::to_string(&StoredEvidence::from(&candidate.evidence))?;
        let now = Utc::now();
        let mut tx = self.0.begin().await?;
        let changed = sqlx::query("UPDATE portals SET state = 'serving', reason = 'Last read verified', last_success_at = ?, next_read_at = ?, recipe = ? WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(now.to_rfc3339()).bind(now.timestamp().saturating_add(interval)).bind(recipe)
            .bind(&job.portal_id).bind(job.generation).execute(&mut *tx).await?.rows_affected();
        if changed == 0 {
            return Ok(());
        }
        sqlx::query("INSERT INTO read_evidence(portal_id, generation, payload) VALUES(?, ?, ?) ON CONFLICT(portal_id, generation) DO UPDATE SET payload = excluded.payload")
            .bind(&job.portal_id).bind(job.generation).bind(evidence).execute(&mut *tx).await?;
        sqlx::query("UPDATE portal_retry SET temporary_failures = 0, last_repair_at = NULL WHERE portal_id = ? AND generation = ?")
            .bind(&job.portal_id).bind(job.generation).execute(&mut *tx).await?;

        let previous: Vec<(String, String)> = sqlx::query_as(
            "SELECT tender_key, payload FROM tenders WHERE portal_id = ? AND generation = ?",
        )
        .bind(&job.portal_id)
        .bind(job.generation)
        .fetch_all(&mut *tx)
        .await?;
        let mut previous: BTreeMap<String, PublishedTender> = previous
            .into_iter()
            .map(|(key, payload)| Ok((key, serde_json::from_str(&payload)?)))
            .collect::<Result<_>>()?;
        let mut changed_tenders = Vec::new();
        for mut tender in candidate.tenders {
            let key = tender.key().context("tender lacks a stable identity")?;
            tender.close_if_due(now);
            if let Some(old) = previous.remove(&key) {
                if old.tender.status == TenderStatus::Withdrawn
                    && tender.status == TenderStatus::Closed
                {
                    tender.status = TenderStatus::Withdrawn;
                }
                if tender == old.tender {
                    continue;
                }
            }
            changed_tenders.push((key, tender));
        }
        // A complete read closes the open tenders it no longer lists. A windowed read leaves
        // out open tenders not updated within its window, so it closes them only by deadline.
        for (key, old) in previous {
            if old.tender.status == TenderStatus::Open {
                let mut tender = old.tender;
                if candidate.evidence.windowed {
                    tender.close_if_due(now);
                } else {
                    tender.status = TenderStatus::Closed;
                }
                if tender.status == TenderStatus::Closed {
                    changed_tenders.push((key, tender));
                }
            }
        }
        let changed = changed_tenders.len();
        for (key, tender) in changed_tenders {
            for doc in &tender.documents {
                sqlx::query("INSERT OR IGNORE INTO portal_files(portal_id, generation, sha256) VALUES(?, ?, ?)")
                    .bind(&job.portal_id).bind(job.generation).bind(&doc.sha256).execute(&mut *tx).await?;
            }
            let payload = serde_json::to_string(&PublishedTender {
                tender,
                last_changed_at: now,
            })?;
            sqlx::query("INSERT INTO tenders(portal_id, generation, tender_key, payload) VALUES(?, ?, ?, ?) ON CONFLICT(portal_id, generation, tender_key) DO UPDATE SET payload = excluded.payload")
                .bind(&job.portal_id).bind(job.generation).bind(key).bind(payload).execute(&mut *tx).await?;
        }
        let held: i64 = sqlx::query_scalar("UPDATE portals SET tender_count = (SELECT COUNT(*) FROM tenders WHERE portal_id = ? AND generation = ?) WHERE id = ? RETURNING tender_count")
            .bind(&job.portal_id).bind(job.generation).bind(&job.portal_id).fetch_one(&mut *tx).await?;
        let changes = match changed {
            0 => "no changes".to_owned(),
            1 => "1 changed".to_owned(),
            count => format!("{count} changed"),
        };
        let reason = format!(
            "{note}. {} held, {changes}",
            crate::domain::tenders(usize::try_from(held).unwrap_or(0))
        );
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'ok', reason = ? WHERE id = ?")
            .bind(now.to_rfc3339())
            .bind(reason)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Open tenders per portal, counted the way `tenders` would serve them: a serving portal's
    /// tenders past their deadline count as closed. Dashboard only; portals without open
    /// tenders are absent.
    pub async fn open_tender_counts(&self) -> Result<BTreeMap<String, usize>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT p.id, p.state, t.payload FROM portals p JOIN tenders t ON t.portal_id = p.id AND t.generation = p.generation WHERE p.deleted = 0")
            .fetch_all(&self.0)
            .await?;
        let now = Utc::now();
        let mut counts = BTreeMap::new();
        for (id, state, payload) in rows {
            let mut item: PublishedTender = serde_json::from_str(&payload)?;
            if state == "serving" {
                item.tender.close_if_due(now);
            }
            if item.tender.status == TenderStatus::Open {
                *counts.entry(id).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    pub async fn tenders(
        &self,
        id: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Option<Vec<PublishedTender>>> {
        let mut tx = self.0.begin().await?;
        let portal: Option<(i64, String)> =
            sqlx::query_as("SELECT generation, state FROM portals WHERE id = ? AND deleted = 0")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some((generation, state)) = portal else {
            return Ok(None);
        };
        let payloads: Vec<(String, String)> = sqlx::query_as("SELECT tender_key, payload FROM tenders WHERE portal_id = ? AND generation = ? ORDER BY tender_key")
            .bind(id).bind(generation).fetch_all(&mut *tx).await?;
        let now = Utc::now();
        let mut tenders = Vec::with_capacity(payloads.len());
        for (key, payload) in payloads {
            let mut item: PublishedTender = serde_json::from_str(&payload)?;
            if state == "serving" && item.tender.status == TenderStatus::Open {
                item.tender.close_if_due(now);
                if item.tender.status == TenderStatus::Closed {
                    item.last_changed_at = now;
                    sqlx::query("UPDATE tenders SET payload = ? WHERE portal_id = ? AND generation = ? AND tender_key = ?")
                        .bind(serde_json::to_string(&item)?).bind(id).bind(generation).bind(key).execute(&mut *tx).await?;
                }
            }
            if since.is_none_or(|since| item.last_changed_at > since) {
                tenders.push(item);
            }
        }
        tx.commit().await?;
        Ok(Some(tenders))
    }

    pub async fn retry_state(&self, job: &Job) -> Result<RetryState> {
        Ok(sqlx::query_as("SELECT temporary_failures, last_repair_at, manual_run FROM portal_retry WHERE portal_id = ? AND generation = ?")
            .bind(&job.portal_id).bind(job.generation).fetch_optional(&self.0).await?
            .unwrap_or_default())
    }

    /// Records a repair that ran out of attempts, which starts the repair cooldown.
    pub async fn record_repair(&self, job: &Job) -> Result<()> {
        sqlx::query("INSERT INTO portal_retry(portal_id, generation, last_repair_at) VALUES(?, ?, ?) ON CONFLICT(portal_id, generation) DO UPDATE SET last_repair_at = excluded.last_repair_at")
            .bind(&job.portal_id).bind(job.generation).bind(Utc::now().timestamp()).execute(&self.0).await?;
        Ok(())
    }

    /// Records a temporary failure and schedules a retry with backoff.
    ///
    /// The portal keeps its state and its published tenders until `limit` is reached: the
    /// consecutive failure count, or the time since the last success (or, before one, since
    /// the first read began). Past that limit it becomes failed and keeps being retried at the
    /// regular interval. A repair cut short by a temporary failure also ends failed, because
    /// the read that started it had already shown the portal broken.
    pub async fn defer(
        &self,
        job: &Job,
        run_id: &str,
        failure: &str,
        retry_in: impl Fn(i64) -> i64,
        limit: OutageLimit,
    ) -> Result<()> {
        let now = Utc::now();
        let mut tx = self.0.begin().await?;
        let failures: i64 = sqlx::query_scalar("INSERT INTO portal_retry(portal_id, generation, temporary_failures) VALUES(?, ?, 1) ON CONFLICT(portal_id, generation) DO UPDATE SET temporary_failures = temporary_failures + 1 RETURNING temporary_failures")
            .bind(&job.portal_id).bind(job.generation).fetch_one(&mut *tx).await?;
        let portal: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT state, last_success_at FROM portals WHERE id = ? AND generation = ? AND deleted = 0",
        )
        .bind(&job.portal_id)
        .bind(job.generation)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((state, last_success)) = portal else {
            return Ok(());
        };
        let since = match last_success {
            Some(time) => Some(time),
            None => {
                sqlx::query_scalar(
                    "SELECT MIN(started_at) FROM runs WHERE portal_id = ? AND generation = ?",
                )
                .bind(&job.portal_id)
                .bind(job.generation)
                .fetch_one(&mut *tx)
                .await?
            }
        };
        let outage = since
            .and_then(|time| DateTime::parse_from_rfc3339(&time).ok())
            .map_or(0, |time| (now - time.with_timezone(&Utc)).num_seconds());
        let exhausted = failures >= limit.failures || outage >= limit.seconds;
        let next = now.timestamp().saturating_add(retry_in(failures));
        let at = DateTime::from_timestamp(next, 0)
            .unwrap_or(now)
            .to_rfc3339();
        let (state, reason) = match state.as_str() {
            "failed" => (state, None),
            "healing" => (
                "failed".to_owned(),
                Some(format!(
                    "Repair interrupted by a temporary failure: {failure}. Retrying at {at}"
                )),
            ),
            _ if exhausted => (
                "failed".to_owned(),
                Some(format!(
                    "Portal unavailable after {failures} attempt(s): {failure}. Retrying at {at}"
                )),
            ),
            "integrating" => (
                state,
                Some(format!(
                    "Attach paused by a temporary failure: {failure}. Retrying at {at}"
                )),
            ),
            _ => (
                state,
                Some(format!(
                    "Last read failed temporarily: {failure}. Retrying at {at}"
                )),
            ),
        };
        let run_reason = reason
            .clone()
            .unwrap_or_else(|| format!("Portal still unavailable: {failure}. Retrying at {at}"));
        sqlx::query("UPDATE portals SET state = ?, reason = COALESCE(?, reason), next_read_at = ? WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(state).bind(reason).bind(next).bind(&job.portal_id).bind(job.generation).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'failed', reason = ? WHERE id = ?")
            .bind(now.to_rfc3339())
            .bind(run_reason)
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deletes the stored decisions of the job's portal generation, before a fresh attach.
    pub async fn clear_decisions(&self, job: &Job) -> Result<()> {
        sqlx::query("DELETE FROM connection_decisions WHERE portal_id = ? AND generation = ?")
            .bind(&job.portal_id)
            .bind(job.generation)
            .execute(&self.0)
            .await?;
        Ok(())
    }

    /// Stores a connection decision under its attempt number for the job's portal generation.
    /// Nothing is stored for a portal that was removed meanwhile.
    pub async fn record_decision(&self, job: &Job, decision: &ConnectionDecision) -> Result<()> {
        sqlx::query("INSERT OR REPLACE INTO connection_decisions(portal_id, generation, attempt, payload) SELECT id, generation, ?, ? FROM portals WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(i64::from(decision.attempt))
            .bind(serde_json::to_string(decision)?)
            .bind(&job.portal_id)
            .bind(job.generation)
            .execute(&self.0)
            .await?;
        Ok(())
    }

    /// The decision with the highest attempt for the portal's current generation. `None` when the
    /// portal is unknown or no decision was stored yet.
    pub async fn latest_decision(&self, portal_id: &str) -> Result<Option<ConnectionDecision>> {
        let payload: Option<String> = sqlx::query_scalar("SELECT d.payload FROM connection_decisions d JOIN portals p ON p.id = d.portal_id AND p.generation = d.generation WHERE p.id = ? AND p.deleted = 0 ORDER BY d.attempt DESC LIMIT 1")
            .bind(portal_id)
            .fetch_optional(&self.0)
            .await?;
        Ok(payload
            .map(|payload| serde_json::from_str(&payload))
            .transpose()?)
    }

    /// The listing evidence of the portal's last verified read, if one was stored.
    pub async fn evidence(&self, job: &Job) -> Result<Option<StoredEvidence>> {
        let payload: Option<String> = sqlx::query_scalar(
            "SELECT payload FROM read_evidence WHERE portal_id = ? AND generation = ?",
        )
        .bind(&job.portal_id)
        .bind(job.generation)
        .fetch_optional(&self.0)
        .await?;
        Ok(payload
            .map(|payload| serde_json::from_str(&payload))
            .transpose()?)
    }

    pub async fn runs(&self, id: &str) -> Result<Option<Vec<Run>>> {
        let mut tx = self.0.begin().await?;
        let generation: Option<i64> =
            sqlx::query_scalar("SELECT generation FROM portals WHERE id = ? AND deleted = 0")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(generation) = generation else {
            return Ok(None);
        };
        let runs = sqlx::query_as("SELECT id, kind, started_at, finished_at, result FROM runs WHERE portal_id = ? AND generation = ? AND started_at IS NOT NULL ORDER BY rowid")
            .bind(id).bind(generation).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(runs))
    }

    /// The portal's runs with their recorded reasons, for the dashboard.
    pub async fn run_history(&self, id: &str) -> Result<Option<Vec<RunHistory>>> {
        let mut tx = self.0.begin().await?;
        let generation: Option<i64> =
            sqlx::query_scalar("SELECT generation FROM portals WHERE id = ? AND deleted = 0")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
        let Some(generation) = generation else {
            return Ok(None);
        };
        let runs = sqlx::query_as("SELECT id, kind, started_at, finished_at, result, reason FROM runs WHERE portal_id = ? AND generation = ? AND started_at IS NOT NULL ORDER BY rowid")
            .bind(id).bind(generation).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(runs))
    }

    pub async fn has_file(&self, sha: &str) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM portal_files f JOIN portals p ON p.id = f.portal_id AND p.generation = f.generation WHERE p.deleted = 0 AND f.sha256 = ?)")
            .bind(sha).fetch_one(&self.0).await?)
    }

    pub async fn record_usage(
        &self,
        job: &Job,
        operation: ModelOperation,
        model: &str,
        usage: ModelUsage,
    ) -> Result<()> {
        sqlx::query("INSERT INTO model_usage(portal_id, generation, operation, model, input_tokens, cached_input_tokens, output_tokens) SELECT id, generation, ?, ?, ?, ?, ? FROM portals WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(operation.as_str()).bind(model).bind(usage.input).bind(usage.cached_input).bind(usage.output)
            .bind(&job.portal_id).bind(job.generation).execute(&self.0).await?;
        Ok(())
    }

    pub async fn usage_totals(
        &self,
        job: &Job,
        operation: ModelOperation,
    ) -> Result<(i64, ModelUsage)> {
        let (calls, input, cached_input, output) = sqlx::query_as(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens), 0), COALESCE(SUM(cached_input_tokens), 0), COALESCE(SUM(output_tokens), 0)
             FROM model_usage WHERE portal_id = ? AND generation = ? AND operation = ?",
        )
        .bind(&job.portal_id)
        .bind(job.generation)
        .bind(operation.as_str())
        .fetch_one(&self.0)
        .await?;
        Ok((
            calls,
            ModelUsage {
                input,
                cached_input,
                output,
            },
        ))
    }
}

/// Brings a database created by an earlier version up to `schema.sql`, whose
/// `CREATE TABLE IF NOT EXISTS` leaves existing tables as they were.
async fn migrate(pool: &SqlitePool) -> Result<()> {
    let has_reason: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('runs') WHERE name = 'reason')",
    )
    .fetch_one(pool)
    .await?;
    if !has_reason {
        sqlx::query("ALTER TABLE runs ADD COLUMN reason TEXT")
            .execute(pool)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn opening_a_database_from_before_run_reasons_adds_the_column() {
        let data = tempfile::TempDir::new().unwrap();
        let path = data.path().join("old.sqlite");
        {
            let pool = SqlitePoolOptions::new()
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(&path)
                        .create_if_missing(true),
                )
                .await
                .unwrap();
            sqlx::raw_sql("CREATE TABLE runs (id TEXT PRIMARY KEY, portal_id TEXT NOT NULL, generation INTEGER NOT NULL, kind TEXT NOT NULL, started_at TEXT, finished_at TEXT, result TEXT); INSERT INTO runs VALUES('r', 'p', 1, 'read', 'x', 'y', 'ok');")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        }
        let store = Store::open(&path).await.unwrap();
        let reason: Option<String> = sqlx::query_scalar("SELECT reason FROM runs WHERE id = 'r'")
            .fetch_one(&store.0)
            .await
            .unwrap();
        assert_eq!(reason, None, "existing runs keep a null reason");
        drop(store);
        Store::open(&path).await.expect("opening again is a no-op");
    }

    #[tokio::test]
    async fn restart_schedules_a_recheck_for_failed_portals_left_unscheduled() {
        let data = tempfile::TempDir::new().unwrap();
        let store = Store::open(&data.path().join("recover.sqlite"))
            .await
            .unwrap();
        let id = store.register("https://example.com/").await.unwrap();
        // How versions before the recovery policy left a failed portal.
        sqlx::query("UPDATE portals SET state = 'failed', next_read_at = NULL WHERE id = ?")
            .bind(&id)
            .execute(&store.0)
            .await
            .unwrap();
        let before = Utc::now().timestamp();
        store.recover(6 * 3600).await.unwrap();
        let next: Option<i64> = sqlx::query_scalar("SELECT next_read_at FROM portals WHERE id = ?")
            .bind(&id)
            .fetch_one(&store.0)
            .await
            .unwrap();
        let next = next.expect("the failed portal is scheduled again");
        assert!((before + 6 * 3600..=Utc::now().timestamp() + 6 * 3600).contains(&next));

        // A portal that already has a scheduled read keeps it.
        sqlx::query("UPDATE portals SET next_read_at = 42 WHERE id = ?")
            .bind(&id)
            .execute(&store.0)
            .await
            .unwrap();
        store.recover(6 * 3600).await.unwrap();
        let kept: Option<i64> = sqlx::query_scalar("SELECT next_read_at FROM portals WHERE id = ?")
            .bind(&id)
            .fetch_one(&store.0)
            .await
            .unwrap();
        assert_eq!(kept, Some(42));
    }

    #[tokio::test]
    async fn latest_decision_is_the_highest_attempt_and_removal_deletes_decisions() {
        let data = tempfile::TempDir::new().unwrap();
        let store = Store::open(&data.path().join("decisions.sqlite"))
            .await
            .unwrap();
        let id = store.register("https://example.com/").await.unwrap();
        let job = store.claim().await.unwrap().unwrap();
        assert!(store.latest_decision(&id).await.unwrap().is_none());
        let candidate = |method| crate::domain::ConnectionCandidate {
            method,
            entry_url: "https://example.com/".into(),
            found_via: "start page".into(),
            http_status: 200,
            content_type: None,
            sample_items: 1,
            fields_seen: Vec::new(),
            has_pagination: false,
            score: 0.5,
        };
        use crate::domain::ConnectionMethod::{Html, Ocds};
        let mut first =
            crate::model::top_ranked(vec![candidate(Html), candidate(Ocds)], 1).unwrap();
        store.record_decision(&job, &first).await.unwrap();
        first.attempt = 2;
        first.chosen = candidate(Ocds);
        first.alternatives.clear();
        store.record_decision(&job, &first).await.unwrap();
        let latest = store.latest_decision(&id).await.unwrap().unwrap();
        assert_eq!((latest.attempt, latest.chosen.method), (2, Ocds));
        assert!(store.latest_decision("unknown").await.unwrap().is_none());

        store.remove(&id).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM connection_decisions")
            .fetch_one(&store.0)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        assert_eq!(store.register("https://example.com/").await.unwrap(), id);
        assert!(store.latest_decision(&id).await.unwrap().is_none());
    }
}
