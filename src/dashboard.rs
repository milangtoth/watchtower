use crate::api::Api;
use axum::{
    Router,
    http::{StatusCode, header},
    response::IntoResponse,
    routing::get,
};
use std::path::PathBuf;

const JAVASCRIPT: [(header::HeaderName, &str); 1] =
    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")];

pub(crate) fn routes() -> Router<Api> {
    Router::new()
        .route(
            "/dashboard/assistant.js",
            get(|| async { (JAVASCRIPT, include_str!("dashboard/assistant.js")) }),
        )
        .route(
            "/dashboard/tender-search.js",
            get(|| async { (JAVASCRIPT, include_str!("dashboard/tender-search.js")) }),
        )
        .route("/dashboard/elevenlabs-sdk.js", get(voice_sdk))
}

async fn voice_sdk() -> Result<impl IntoResponse, StatusCode> {
    let directory = std::env::var_os("WATCHTOWER_DASHBOARD_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("src/dashboard"));
    let content = tokio::fs::read(directory.join("elevenlabs-sdk.js"))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((JAVASCRIPT, content))
}
