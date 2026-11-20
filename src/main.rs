use anyhow::{Context, Result};
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;
use watchtower::{
    api::{self, Api},
    db::Store,
    fetch::Fetcher,
    model::OpenAi,
    worker::{Jobs, Worker},
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
    store.recover().await?;
    let interval = std::env::var("WATCHTOWER_READ_INTERVAL")
        .ok()
        .map(|value| value.parse::<i64>())
        .transpose()
        .context("WATCHTOWER_READ_INTERVAL must be an integer")?
        .unwrap_or(900);
    anyhow::ensure!(interval > 0, "WATCHTOWER_READ_INTERVAL must be positive");

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    let stop = CancellationToken::new();
    let jobs = Jobs::default();
    let worker = Worker {
        store: store.clone(),
        jobs: jobs.clone(),
        interval,
        fetcher: Fetcher::new(files.clone())?,
        model: Arc::new(OpenAi::new(store.clone())?),
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
