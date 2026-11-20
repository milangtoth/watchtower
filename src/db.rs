use crate::domain::{
    CandidateRead, ModelOperation, ModelUsage, Portal, PublishedTender, Run, TenderStatus,
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
        if let Some(run_id) = existing {
            return Ok(Some(run_id));
        }
        let run_id = Uuid::new_v4().to_string();
        sqlx::query(INSERT_RUN)
            .bind(&run_id)
            .bind(id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(run_id))
    }

    pub async fn remove(&self, id: &str) -> Result<Option<i64>> {
        let mut tx = self.0.begin().await?;
        let generation: Option<i64> = sqlx::query_scalar("UPDATE portals SET deleted = 1, next_read_at = NULL WHERE id = ? AND deleted = 0 RETURNING generation")
            .bind(id).fetch_optional(&mut *tx).await?;
        if let Some(generation) = generation {
            for table in ["runs", "tenders", "portal_files", "model_usage"] {
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

    pub async fn recover(&self) -> Result<()> {
        let mut tx = self.0.begin().await?;
        sqlx::query("UPDATE portals SET next_read_at = 0 WHERE deleted = 0 AND (state IN ('integrating', 'healing') OR EXISTS (SELECT 1 FROM runs r WHERE r.portal_id = portals.id AND r.generation = portals.generation AND r.started_at IS NOT NULL AND r.finished_at IS NULL))")
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'failed' WHERE started_at IS NOT NULL AND finished_at IS NULL")
            .bind(Utc::now().to_rfc3339()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn auto_refresh(&self) -> Result<bool> {
        Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM preferences WHERE name = 'auto_refresh' AND enabled = 1)")
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
        let portals: Vec<(String, i64)> = sqlx::query_as("SELECT id, generation FROM portals p WHERE EXISTS(SELECT 1 FROM preferences WHERE name = 'auto_refresh' AND enabled = 1) AND deleted = 0 AND next_read_at <= ? AND NOT EXISTS (SELECT 1 FROM runs r WHERE r.portal_id = p.id AND r.generation = p.generation AND r.finished_at IS NULL)")
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
            let state = if job.last_success_at.is_none() {
                "integrating"
            } else {
                "healing"
            };
            sqlx::query("UPDATE portals SET state = ?, reason = 'Retrying portal read' WHERE id = ? AND state = 'failed'")
                .bind(state).bind(&job.portal_id).execute(&mut *tx).await?;
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

    pub async fn begin_repair(&self, job: &Job, reason: &str) -> Result<Option<String>> {
        let mut tx = self.0.begin().await?;
        let changed = sqlx::query("UPDATE portals SET state = 'healing', reason = ? WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(reason).bind(&job.portal_id).bind(job.generation).execute(&mut *tx).await?.rows_affected();
        if changed == 0 {
            return Ok(None);
        }
        let now = Utc::now().to_rfc3339();
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'failed' WHERE id = ?")
            .bind(&now)
            .bind(&job.id)
            .execute(&mut *tx)
            .await?;
        let id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO runs(id, portal_id, generation, kind, started_at) VALUES(?, ?, ?, 'repair', ?)")
            .bind(&id).bind(&job.portal_id).bind(job.generation).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(id))
    }

    pub async fn fail(&self, job: &Job, run_id: &str, reason: &str) -> Result<()> {
        let mut tx = self.0.begin().await?;
        sqlx::query("UPDATE portals SET state = 'failed', reason = ?, next_read_at = NULL WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(reason).bind(&job.portal_id).bind(job.generation).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'failed' WHERE id = ?")
            .bind(Utc::now().to_rfc3339())
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn fail_job(&self, job: &Job, reason: &str) -> Result<()> {
        let run: Option<String> = sqlx::query_scalar("SELECT id FROM runs WHERE portal_id = ? AND generation = ? AND started_at IS NOT NULL AND finished_at IS NULL")
            .bind(&job.portal_id).bind(job.generation).fetch_optional(&self.0).await?;
        if let Some(run) = run {
            self.fail(job, &run, reason).await?;
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
    ) -> Result<()> {
        anyhow::ensure!(
            candidate.evidence.terminal_page,
            "cannot publish incomplete pagination"
        );
        let now = Utc::now();
        let mut tx = self.0.begin().await?;
        let changed = sqlx::query("UPDATE portals SET state = 'serving', reason = 'Last read verified', last_success_at = ?, next_read_at = ?, recipe = ? WHERE id = ? AND generation = ? AND deleted = 0")
            .bind(now.to_rfc3339()).bind(now.timestamp().saturating_add(interval)).bind(recipe)
            .bind(&job.portal_id).bind(job.generation).execute(&mut *tx).await?.rows_affected();
        if changed == 0 {
            return Ok(());
        }

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
        for (key, old) in previous {
            if old.tender.status == TenderStatus::Open {
                let mut tender = old.tender;
                tender.status = TenderStatus::Closed;
                changed_tenders.push((key, tender));
            }
        }
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
        sqlx::query("UPDATE portals SET tender_count = (SELECT COUNT(*) FROM tenders WHERE portal_id = ? AND generation = ?) WHERE id = ?")
            .bind(&job.portal_id).bind(job.generation).bind(&job.portal_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE runs SET finished_at = ?, result = 'ok' WHERE id = ?")
            .bind(now.to_rfc3339())
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
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
}
