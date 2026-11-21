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

/// Static dashboard files compiled into the binary: path, content type, bytes.
const ASSETS: &[(&str, &str, &[u8])] = &[
    (
        "/dashboard.css",
        "text/css; charset=utf-8",
        include_bytes!("dashboard.css"),
    ),
    (
        "/design/inter-latin.woff2",
        "font/woff2",
        include_bytes!("assets/design/inter-latin.woff2"),
    ),
    (
        "/design/eb-garamond-latin.woff2",
        "font/woff2",
        include_bytes!("assets/design/eb-garamond-latin.woff2"),
    ),
    (
        "/design/inter-OFL.txt",
        "text/plain; charset=utf-8",
        include_bytes!("assets/design/inter-OFL.txt"),
    ),
    (
        "/design/eb-garamond-OFL.txt",
        "text/plain; charset=utf-8",
        include_bytes!("assets/design/eb-garamond-OFL.txt"),
    ),
    (
        "/favicon.ico",
        "image/x-icon",
        include_bytes!("assets/favicon.ico"),
    ),
    (
        "/favicon.svg",
        "image/svg+xml",
        include_bytes!("assets/favicon.svg"),
    ),
    (
        "/favicon-16x16.png",
        "image/png",
        include_bytes!("assets/favicon-16x16.png"),
    ),
    (
        "/favicon-32x32.png",
        "image/png",
        include_bytes!("assets/favicon-32x32.png"),
    ),
    (
        "/apple-touch-icon.png",
        "image/png",
        include_bytes!("assets/apple-touch-icon.png"),
    ),
    (
        "/icon-192.png",
        "image/png",
        include_bytes!("assets/icon-192.png"),
    ),
    (
        "/icon-512.png",
        "image/png",
        include_bytes!("assets/icon-512.png"),
    ),
    (
        "/site.webmanifest",
        "application/manifest+json",
        include_bytes!("assets/site.webmanifest"),
    ),
    (
        "/robots.txt",
        "text/plain; charset=utf-8",
        include_bytes!("assets/robots.txt"),
    ),
];

pub fn router(api: Api) -> Router {
    let dashboard = get(|| async { Html(include_str!("dashboard.html")) });
    let mut router = Router::new().route("/", dashboard);
    for &(path, content_type, body) in ASSETS {
        router = router.route(path, get(move || async move { asset(content_type, body) }));
    }
    router
        .route(
            "/settings/auto-refresh",
            get(refresh_preference).post(update_refresh_preference),
        )
        .route("/portals", get(list).post(register))
        .route("/portals/{id}", get(portal).delete(remove))
        .route("/portals/{id}/refresh", axum::routing::post(refresh))
        .route("/portals/{id}/tenders", get(tenders))
        .route("/portals/{id}/runs", get(runs))
        .route("/portals/{id}/connection", get(connection))
        .route("/files/{id}", get(file))
        .merge(crate::dashboard::routes())
        .merge(crate::voice::router())
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "Not found".into()) })
        .method_not_allowed_fallback(|| async {
            ApiError(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed".into())
        })
        .with_state(api)
}

/// One static dashboard file, cached for an hour.
fn asset(content_type: &'static str, body: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        body,
    )
        .into_response()
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

#[derive(Deserialize, serde::Serialize)]
struct RefreshPreference {
    enabled: bool,
}

async fn refresh_preference(State(api): State<Api>) -> Result<Json<RefreshPreference>, ApiError> {
    Ok(Json(RefreshPreference {
        enabled: api.store.auto_refresh().await?,
    }))
}

async fn update_refresh_preference(
    State(api): State<Api>,
    payload: Result<Json<RefreshPreference>, JsonRejection>,
) -> Result<Json<RefreshPreference>, ApiError> {
    let preference = payload.map_err(|err| ApiError::invalid(err.body_text()))?;
    api.store.set_auto_refresh(preference.enabled).await?;
    Ok(preference)
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
        .map(changed_since)
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

/// Parses `changedSince`. An unencoded `+` offset arrives as a space, since a query string
/// decodes `+` that way, so a value that fails as sent is retried with its last space as `+`.
fn changed_since(value: &str) -> Result<DateTime<chrono::FixedOffset>, chrono::ParseError> {
    DateTime::parse_from_rfc3339(value).or_else(|error| match value.rsplit_once(' ') {
        Some((time, offset)) => DateTime::parse_from_rfc3339(&format!("{time}+{offset}")),
        None => Err(error),
    })
}

async fn runs(
    State(api): State<Api>,
    ExtractPath(id): ExtractPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    Ok(Json(
        api.store.runs(&id).await?.ok_or_else(ApiError::missing)?,
    ))
}

/// The portal's latest connection decision, or `null` before one exists. Dashboard only; not
/// part of the challenge API.
async fn connection(
    State(api): State<Api>,
    ExtractPath(id): ExtractPath<String>,
) -> Result<impl IntoResponse, ApiError> {
    api.store.portal(&id).await?.ok_or_else(ApiError::missing)?;
    Ok(Json(api.store.latest_decision(&id).await?))
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
