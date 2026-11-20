use crate::{db::Store, fetch::public_http, worker::Jobs};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        Path as ExtractPath, Query, State,
        rejection::{JsonRejection, QueryRejection},
    },
    http::{StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use chrono::{DateTime, Utc};
use reqwest::Url;
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};
use tokio_util::io::ReaderStream;

#[derive(Clone)]
pub struct Api {
    pub store: Store,
    pub jobs: Jobs,
    pub files: Arc<PathBuf>,
}

pub fn router(api: Api) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("dashboard.html")) }))
        .route(
            "/dashboard.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("dashboard.css"),
                )
            }),
        )
        .route("/portals", get(list).post(register))
        .route("/portals/{id}", get(portal).delete(remove))
        .route("/portals/{id}/refresh", axum::routing::post(refresh))
        .route("/portals/{id}/tenders", get(tenders))
        .route("/portals/{id}/runs", get(runs))
        .route("/files/{id}", get(file))
        .with_state(api)
}

struct ApiError(StatusCode, String);

impl ApiError {
    fn missing() -> Self {
        Self(StatusCode::NOT_FOUND, "Unknown ID".into())
    }

    fn invalid(reason: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, reason.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        tracing::error!(%error, "HTTP request failed");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Internal service error".into(),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"detail": self.1}))).into_response()
    }
}

#[derive(Deserialize)]
struct Registration {
    url: String,
}

#[derive(Deserialize)]
struct TenderQuery {
    #[serde(rename = "changedSince")]
    changed_since: Option<String>,
}

async fn register(
    State(api): State<Api>,
    payload: Result<Json<Registration>, JsonRejection>,
) -> Result<Response, ApiError> {
    let Json(body) = payload.map_err(|err| ApiError::invalid(err.body_text()))?;
    Url::parse(&body.url)
        .ok()
        .filter(public_http)
        .ok_or_else(|| ApiError::invalid("url must be HTTP(S) without credentials"))?;
    let id = api.store.register(&body.url).await?;
    api.jobs.wake();
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))).into_response())
}

async fn list(State(api): State<Api>) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(api.store.list().await?))
}

async fn portal(
    State(api): State<Api>,
    ExtractPath(id): ExtractPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        api.store.portal(&id).await?.ok_or_else(ApiError::missing)?,
    ))
}

async fn refresh(
    State(api): State<Api>,
    ExtractPath(id): ExtractPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    let id = api
        .store
        .refresh(&id)
        .await?
        .ok_or_else(ApiError::missing)?;
    api.jobs.wake();
    Ok((StatusCode::ACCEPTED, Json(json!({ "runId": id }))))
}

async fn remove(
    State(api): State<Api>,
    ExtractPath(id): ExtractPath<String>,
) -> Result<StatusCode, ApiError> {
    let generation = api.store.remove(&id).await?.ok_or_else(ApiError::missing)?;
    api.jobs.cancel(id, generation).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn tenders(
    State(api): State<Api>,
    ExtractPath(id): ExtractPath<String>,
    query: Result<Query<TenderQuery>, QueryRejection>,
) -> Result<impl IntoResponse, ApiError> {
    let Query(query) = query.map_err(|error| ApiError::invalid(error.body_text()))?;
    let since = query
        .changed_since
        .as_deref()
        .map(DateTime::parse_from_rfc3339)
        .transpose()
        .map_err(|_| ApiError::invalid("changedSince must be an RFC 3339 date-time with offset"))?
        .map(|value| value.with_timezone(&Utc));
    Ok(Json(
        api.store
            .tenders(&id, since)
            .await?
            .ok_or_else(ApiError::missing)?,
    ))
}

async fn runs(
    State(api): State<Api>,
    ExtractPath(id): ExtractPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        api.store.runs(&id).await?.ok_or_else(ApiError::missing)?,
    ))
}

async fn file(
    State(api): State<Api>,
    ExtractPath(id): ExtractPath<String>,
) -> Result<Response, ApiError> {
    if id.len() != 64
        || !id.bytes().all(|ch| ch.is_ascii_hexdigit())
        || !api.store.has_file(&id).await?
    {
        return Err(ApiError::missing());
    }
    let file = tokio::fs::File::open(api.files.join(&id))
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                ApiError::missing()
            } else {
                anyhow::Error::from(err).into()
            }
        })?;
    let len = file.metadata().await.map_err(anyhow::Error::from)?.len();
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".into()),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CONTENT_DISPOSITION, "attachment".into()),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response())
}
