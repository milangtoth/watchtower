use anyhow::{Context, Result};
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;
use watchtower::{
    api::{self, Api},
    db::Store,
    fetch::Fetcher,
    model::OpenAi,
    worker::{Jobs, Recovery, Worker},
};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let data =
        PathBuf::from(std::env::var_os("WATCHTOWER_DATA_DIR").unwrap_or_else(|| "./data".into()));
    let files = data.join("files");
    tokio::fs::create_dir_all(&files)
        .await
        .context("Could not create the data directory")?;
    let store = Store::open(&data.join("watchtower.sqlite3")).await?;
    let recovery = Recovery::default();
    store.recover(recovery.recheck).await?;
    let interval = std::env::var("WATCHTOWER_READ_INTERVAL")
        .ok()
        .map(|value| value.parse::<i64>())
        .transpose()
        .context("WATCHTOWER_READ_INTERVAL must be an integer")?
        .unwrap_or(900);
    anyhow::ensure!(interval > 0, "WATCHTOWER_READ_INTERVAL must be positive");
    let concurrency = std::env::var("WATCHTOWER_CONCURRENCY")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("WATCHTOWER_CONCURRENCY must be a positive integer")?
        .unwrap_or(watchtower::worker::CONCURRENCY);
    anyhow::ensure!(concurrency > 0, "WATCHTOWER_CONCURRENCY must be positive");
    let model_calls = std::env::var("WATCHTOWER_MODEL_CONCURRENCY")
        .ok()
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("WATCHTOWER_MODEL_CONCURRENCY must be a positive integer")?
        .unwrap_or(watchtower::model::MODEL_CONCURRENCY);
    anyhow::ensure!(
        model_calls > 0,
        "WATCHTOWER_MODEL_CONCURRENCY must be positive"
    );
    // Development-only switches. Both are off unless set, and the Dockerfile sets neither.
    let raw_rps = std::env::var("WATCHTOWER_DEV_RPS").ok();
    let rps = watchtower::fetch::dev_rps(raw_rps.as_deref())?;
    if rps > 1.0 {
        tracing::warn!(
            requested = raw_rps.as_deref().unwrap_or_default(),
            rps,
            max = watchtower::fetch::MAX_DEV_RPS,
            "WATCHTOWER_DEV_RPS raises the per-portal request rate above 1 per second; development only"
        );
    }
    let mut fetcher = Fetcher::new(files.clone())?.with_rate(rps);
    if let Some(cache) = std::env::var_os("WATCHTOWER_DEV_CACHE").filter(|value| !value.is_empty())
    {
        let cache = PathBuf::from(cache);
        tracing::warn!(
            cache = %cache.display(),
            "WATCHTOWER_DEV_CACHE answers repeated portal requests from disk; development only"
        );
        fetcher = fetcher.with_cache(cache)?;
    }

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    let stop = CancellationToken::new();
    let jobs = Jobs::default();
    let worker = Worker {
        store: store.clone(),
        jobs: jobs.clone(),
        interval,
        fetcher,
        model: Arc::new(OpenAi::new(store.clone())?.with_concurrency(model_calls)),
        recovery,
        concurrency,
    };
    let handle = tokio::spawn(worker.run(stop.clone()));
    let app = api::router(Api {
        store,
        jobs,
        files: Arc::new(files),
    });
    tracing::info!("Watchtower is listening on port 8080");
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(stop.clone()))
        .await;
    stop.cancel();
    handle.await?;
    result?;
    Ok(())
}

async fn shutdown_signal(stop: CancellationToken) {
    #[cfg(unix)]
    if let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
        }
        stop.cancel();
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
    stop.cancel();
}
