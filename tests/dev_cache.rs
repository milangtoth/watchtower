//! Development-only switches on the shared client: `WATCHTOWER_DEV_RPS` and
//! `WATCHTOWER_DEV_CACHE`.

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::{Next, from_fn_with_state},
    response::{Html, IntoResponse, Redirect},
    routing::get,
};
use reqwest::Url;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tempfile::TempDir;
use watchtower::{
    domain::ReadFailure,
    fetch::{Fetcher, MAX_DEV_RPS, dev_rps},
    recipe::Recipe,
};

const SPEC: &[u8] = b"%PDF-1.4\nSpecification\n%%EOF\n";
const ANNEX: &[u8] = b"%PDF-1.4\nPrice annex\n%%EOF\n";

fn bundle() -> Vec<u8> {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    archive.start_file("annex.pdf", options).unwrap();
    archive.write_all(ANNEX).unwrap();
    archive.finish().unwrap().into_inner()
}

/// A portal that counts every request it answers.
async fn portal() -> (Url, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route("/", get(|| async { Redirect::temporary("/list") }))
        .route(
            "/list",
            get(|| async {
                Html(
                    r#"<article class="tender" data-id="A"><h2>Road survey</h2>
                    <section class="attachment"><a href="/files/spec">Specification</a></section>
                    <section class="attachment"><a href="/files/bundle.zip">Bundle</a></section>
                    </article>
                    <article class="tender" data-id="B"><h2>Park works</h2>
                    <section class="attachment"><a href="/files/spec">Specification</a></section>
                    </article>"#,
                )
            }),
        )
        .route(
            "/files/spec",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "application/pdf"),
                        (
                            header::CONTENT_DISPOSITION,
                            "attachment; filename=\"Specification.pdf\"",
                        ),
                    ],
                    SPEC,
                )
            }),
        )
        .route(
            "/files/bundle.zip",
            get(|| async { ([(header::CONTENT_TYPE, "application/zip")], bundle()) }),
        )
        .route(
            "/forbidden",
            get(|| async { (StatusCode::FORBIDDEN, "Forbidden").into_response() }),
        )
        .route(
            "/busy",
            get(|| async {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::RETRY_AFTER, "7")],
                    "Slow down",
                )
                    .into_response()
            }),
        )
        .route(
            "/signin-wall",
            get(|| async {
                Html(
                    r#"<html><head><title>Login</title></head>
                    <body><form><input type="password" name="pw"></form></body></html>"#,
                )
            }),
        )
        .layer(from_fn_with_state(
            hits.clone(),
            |State(hits): State<Arc<AtomicUsize>>, request: Request<Body>, next: Next| async move {
                hits.fetch_add(1, Ordering::SeqCst);
                next.run(request).await
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, hits, server)
}

fn recipe() -> Recipe {
    serde_json::from_value(json!({
        "listing": "html", "items": ".tender",
        "list_fields": {"id": {"path": "", "attribute": "data-id"}, "title": {"path": "h2"}},
        "documents": {"path": ".attachment a", "attribute": "href"}
    }))
    .unwrap()
}

type Delivered = Vec<(Option<String>, Vec<(String, u64, String)>)>;

async fn read(fetcher: &Fetcher, base: &Url) -> Delivered {
    let read = watchtower::recipe::read(&fetcher.portal(base), base, &recipe())
        .await
        .unwrap();
    read.tenders
        .into_iter()
        .map(|tender| {
            let documents = tender
                .documents
                .into_iter()
                .map(|document| (document.file_name, document.size_bytes, document.sha256))
                .collect();
            (tender.id, documents)
        })
        .collect()
}

fn entries(directory: &Path) -> usize {
    std::fs::read_dir(directory).unwrap().count()
}

#[tokio::test]
async fn second_read_with_the_cache_makes_no_requests_and_delivers_the_same_files() {
    let (base, hits, server) = portal().await;
    let cache = TempDir::new().unwrap();
    let first_files = TempDir::new().unwrap();
    let first = Fetcher::new(first_files.path().into())
        .unwrap()
        .with_rate(MAX_DEV_RPS)
        .with_cache(cache.path().into())
        .unwrap();
    let delivered = read(&first, &base).await;
    let requests = hits.load(Ordering::SeqCst);
    assert!(requests > 0);
    assert_eq!(
        delivered,
        [
            (
                Some("A".to_owned()),
                vec![
                    (
                        "Specification.pdf".to_owned(),
                        SPEC.len() as u64,
                        format!("{:x}", Sha256::digest(SPEC))
                    ),
                    (
                        "annex.pdf".to_owned(),
                        ANNEX.len() as u64,
                        format!("{:x}", Sha256::digest(ANNEX))
                    ),
                ]
            ),
            (
                Some("B".to_owned()),
                vec![(
                    "Specification.pdf".to_owned(),
                    SPEC.len() as u64,
                    format!("{:x}", Sha256::digest(SPEC))
                )]
            ),
        ]
    );

    // A fresh client and file store: every document is staged again from the cache alone.
    let second_files = TempDir::new().unwrap();
    let second = Fetcher::new(second_files.path().into())
        .unwrap()
        .with_cache(cache.path().into())
        .unwrap();
    let started = Instant::now();
    assert_eq!(read(&second, &base).await, delivered);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        requests,
        "the cached read reached the portal"
    );
    // Cached answers wait for no rate limit, even at the default one request per second.
    assert!(started.elapsed() < Duration::from_millis(900));
    for (_, documents) in &delivered {
        for (_, size, sha) in documents {
            let staged = std::fs::read(second_files.path().join(sha)).unwrap();
            assert_eq!(staged.len() as u64, *size);
            assert_eq!(&format!("{:x}", Sha256::digest(&staged)), sha);
        }
    }
    server.abort();
}

#[tokio::test]
async fn refused_throttled_and_sign_in_responses_are_not_cached() {
    let (base, hits, server) = portal().await;
    let cache = TempDir::new().unwrap();
    let files = TempDir::new().unwrap();
    let fetcher = Fetcher::new(files.path().into())
        .unwrap()
        .with_rate(MAX_DEV_RPS)
        .with_cache(cache.path().into())
        .unwrap();
    let http = fetcher.portal(&base);
    for round in 1..=2 {
        let forbidden = http
            .page(base.join("forbidden").unwrap())
            .await
            .unwrap_err();
        assert!(
            matches!(forbidden, ReadFailure::Incomplete(_)),
            "{forbidden}"
        );
        let busy = http.page(base.join("busy").unwrap()).await.unwrap_err();
        assert_eq!(busy.retry_after(), Some(Duration::from_secs(7)), "{busy}");
        let wall = http
            .page(base.join("signin-wall").unwrap())
            .await
            .unwrap_err();
        assert!(matches!(wall, ReadFailure::Blocked(_)), "{wall}");
        assert_eq!(hits.load(Ordering::SeqCst), 3 * round);
    }
    assert_eq!(
        entries(cache.path()),
        0,
        "no entry and no leftover temp file"
    );
    server.abort();
}

#[test]
fn rate_override_is_clamped_to_five_per_second() {
    assert_eq!(dev_rps(Some("50")).unwrap(), 5.0);
    assert_eq!(dev_rps(Some("2.5")).unwrap(), 2.5);
    assert!(dev_rps(Some("0")).is_err());
    assert!(dev_rps(Some("-1")).is_err());
    assert!(dev_rps(Some("fast")).is_err());
    assert!(dev_rps(Some("inf")).is_err());
    let files = TempDir::new().unwrap();
    let fetcher = Fetcher::new(files.path().into()).unwrap();
    assert_eq!(
        fetcher.clone().with_rate(50.0).spacing(),
        Duration::from_millis(200)
    );
    assert_eq!(
        fetcher.clone().with_rate(2.0).spacing(),
        Duration::from_millis(500)
    );
    assert_eq!(fetcher.with_rate(0.0).spacing(), Duration::from_secs(1));
}

#[tokio::test]
async fn defaults_keep_one_request_per_second_and_no_cache() {
    assert_eq!(dev_rps(None).unwrap(), 1.0);
    assert_eq!(dev_rps(Some("")).unwrap(), 1.0);
    let (base, hits, server) = portal().await;
    let files = TempDir::new().unwrap();
    let fetcher = Fetcher::new(files.path().into()).unwrap();
    assert_eq!(fetcher.spacing(), Duration::from_secs(1));
    assert!(fetcher.cache_dir().is_none());
    let http = fetcher.portal(&base);
    let list = base.join("list").unwrap();
    let started = Instant::now();
    http.page(list.clone()).await.unwrap();
    http.page(list).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(950));
    assert_eq!(hits.load(Ordering::SeqCst), 2);
    server.abort();
}

#[tokio::test]
async fn raised_rate_spaces_requests_and_redirects_by_the_override() {
    let (base, hits, server) = portal().await;
    let files = TempDir::new().unwrap();
    let fetcher = Fetcher::new(files.path().into()).unwrap().with_rate(50.0);
    let http = fetcher.portal(&base);
    let started = Instant::now();
    // The root redirects to the listing, so this is four requests spaced 200 ms apart.
    http.page(base.clone()).await.unwrap();
    http.page(base.clone()).await.unwrap();
    let elapsed = started.elapsed();
    assert_eq!(hits.load(Ordering::SeqCst), 4);
    assert!(elapsed >= Duration::from_millis(580), "{elapsed:?}");
    assert!(elapsed < Duration::from_millis(950), "{elapsed:?}");
    server.abort();
}
