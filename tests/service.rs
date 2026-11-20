use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode, header},
    middleware::{Next, from_fn_with_state},
    response::{Html, IntoResponse},
    routing::get,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tempfile::TempDir;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use watchtower::{
    api::{self, Api},
    db::Store,
    domain::{CandidateRead, ConnectionDecision, ReadFailure, Review},
    fetch::Fetcher,
    model::{PortalModel, ReasoningContext},
    recipe::Recipe,
    worker::{Jobs, Recovery, Worker},
};

const DOCUMENT: &[u8] = b"%PDF-1.4\nFixture procurement document\n%%EOF\n";
const GUIDE: &[u8] = b"%PDF-1.4\nAttachment page guide\n%%EOF\n";
const CHECKLIST: &[u8] = b"%PDF-1.4\nSupervisor checklist\n%%EOF\n";

#[derive(Default)]
struct FakeModel {
    learns: AtomicUsize,
    verifies: AtomicUsize,
    repairing: Notify,
    release: Notify,
}

#[async_trait::async_trait]
impl PortalModel for FakeModel {
    async fn learn(
        &self,
        _: &ReasoningContext<'_>,
        previous: Option<(&Recipe, &str)>,
        _: &ConnectionDecision,
    ) -> Result<Recipe, ReadFailure> {
        self.learns.fetch_add(1, Ordering::SeqCst);
        let selector = if previous.is_some() {
            self.repairing.notify_one();
            self.release.notified().await;
            ".offer"
        } else {
            ".tender"
        };
        Ok(serde_json::from_value(json!({
            "listing": "html", "items": selector,
            "total": {"path": "#total"}, "next": {"path": ".next", "attribute": "href"},
            "list_fields": {"id":{"path":"","attribute":"data-id"}, "title":{"path":"h2"},
                "url":{"path":".detail","attribute":"href"}, "submissionDeadline":{"path":"time","attribute":"datetime"}},
            "direct_documents": {"path":".file","attribute":"href"},
            "document_page": {"path":".attachment-page","attribute":"href"},
            "nested_document_pages": {"path":".attachment-next","attribute":"href"},
            "documents": {"path":".download","attribute":"href"}
        })).unwrap())
    }

    async fn verify(
        &self,
        _: &ReasoningContext<'_>,
        _: &Recipe,
        candidate: &CandidateRead,
        _: &Review,
    ) -> Result<(), ReadFailure> {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        assert!(candidate.evidence.terminal_page);
        assert_eq!(candidate.tenders.len(), 2);
        Ok(())
    }
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |body| Body::from(body.to_string())))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    (
        response.status(),
        to_bytes(response.into_body(), 1_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
}

async fn get_json(app: &Router, path: &str) -> Value {
    let (status, bytes) = call(app, "GET", path, None).await;
    assert_eq!(status, StatusCode::OK, "{path}");
    serde_json::from_slice(&bytes).unwrap()
}

async fn wait_for(app: &Router, path: &str, matches: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(18), async {
        loop {
            let document = get_json(app, path).await;
            if matches(&document) {
                return document;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("service did not reach the expected state")
}

#[derive(Clone)]
struct Fixture {
    layout: Arc<AtomicU8>,
    archive: Arc<Vec<u8>>,
    requests: Arc<Mutex<Vec<Instant>>>,
}

async fn fixture_portal() -> (
    String,
    Fixture,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    archive
        .start_file("docs/spec.pdf", zip::write::SimpleFileOptions::default())
        .unwrap();
    archive.write_all(DOCUMENT).unwrap();
    let fixture = Fixture {
        layout: Arc::new(AtomicU8::new(0)),
        archive: Arc::new(archive.finish().unwrap().into_inner()),
        requests: Arc::new(Mutex::default()),
    };
    let site = Router::new()
        .route("/", get(|State(fixture): State<Fixture>| async move {
            let layout = fixture.layout.load(Ordering::SeqCst);
            let class = if layout == 0 { "tender" } else { "offer" };
            let next = if layout == 2 { "" } else { r#"<a class="next" href="/page2">Next</a>"# };
            Html(format!(r#"<b id="total">2</b><article class="{class}" data-id="A"><h2>Road survey</h2>
                <a class="detail" href="/tenders/a">Detail</a><time datetime="2099-12-09T10:00:00+01:00"></time>
                <a class="file" href="/files/spec.zip">Document</a>
                <a class="file" href="/attachments/bridge">Supervisor Forms</a>
                <a class="attachment-page" href="/attachments/index">Forms and guidance</a>
                <a class="attachment-page" href="/attachments/index#duplicate">Same attachment page</a>
                </article>{next}"#))
        }))
        .route("/page2", get(|State(fixture): State<Fixture>| async move {
            let layout = fixture.layout.load(Ordering::SeqCst);
            let class = if layout == 0 { "tender" } else { "offer" };
            let title = if layout == 0 { "Park works" } else { "Park works, updated" };
            Html(format!(r#"<article class="{class}" data-id="B"><h2>{title}</h2>
                <a class="detail" href="/tenders/b">Detail</a><time datetime="2099-12-09T09:00:00+01:00"></time></article>"#))
        }))
        .route("/files/spec.zip", get(|State(fixture): State<Fixture>| async move {
            if fixture.layout.load(Ordering::SeqCst) == 3 {
                return StatusCode::UNAUTHORIZED.into_response();
            }
            ([(header::CONTENT_TYPE, "application/zip")], fixture.archive.as_ref().clone()).into_response()
        }))
        .route("/attachments/index", get(|| async {
            Html(r#"<main><a class="download" href="../files/spec.zip">Also listed here</a>
                <a class="attachment-next" href="guide">Follow to public guidance</a></main>"#)
        }))
        .route("/attachments/guide", get(|| async {
            Html(r#"<main><a class="download" href="../files/guide.pdf">Download guide</a>
                <a class="attachment-next" href="index">Back to attachments</a></main>"#)
        }))
        .route("/files/guide.pdf", get(|| async {
            ([(header::CONTENT_TYPE, "application/pdf")], GUIDE)
        }))
        .route("/attachments/bridge", get(|| async {
            Html(r#"<main><a href="/copyright">Licence</a>
                <a href="supervisor">Supervisor Forms and Guidance</a></main>"#)
        }))
        .route("/attachments/supervisor", get(|| async {
            Html(r#"<main><a href="../files/checklist.pdf">Download checklist</a></main>"#)
        }))
        .route("/files/checklist.pdf", get(|| async {
            ([(header::CONTENT_TYPE, "application/pdf")], CHECKLIST)
        }))
        .layer(from_fn_with_state(fixture.clone(), |State(fixture): State<Fixture>, request: Request<Body>, next: Next| async move {
            fixture.requests.lock().unwrap().push(Instant::now());
            next.run(request).await
        }))
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, site).await });
    (url, fixture, server)
}

#[tokio::test]
async fn portal_lifecycle_preserves_verified_data_and_serves_document_bytes() {
    let (url, fixture, server) = fixture_portal().await;
    let data = TempDir::new().unwrap();
    let files = Arc::new(data.path().join("files"));
    tokio::fs::create_dir(&*files).await.unwrap();
    let store = Store::open(&data.path().join("test.sqlite")).await.unwrap();
    let jobs = Jobs::default();
    let model = Arc::new(FakeModel::default());
    let stop = CancellationToken::new();
    let worker = tokio::spawn(
        Worker {
            store: store.clone(),
            jobs: jobs.clone(),
            fetcher: Fetcher::new(files.as_ref().clone()).unwrap(),
            model: model.clone(),
            interval: 900,
            recovery: Recovery::default(),
        }
        .run(stop.clone()),
    );
    let app = api::router(Api { store, jobs, files });
    assert_eq!(call(&app, "GET", "/", None).await.0, StatusCode::OK);
    let (status, body) = call(&app, "POST", "/portals", Some(json!({"url":url}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let body: Value = serde_json::from_slice(&body).unwrap();
    let id = body["id"].as_str().unwrap();
    let (status, duplicate) = call(&app, "POST", "/portals", Some(json!({"url":url}))).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(serde_json::from_slice::<Value>(&duplicate).unwrap(), body);
    let path = format!("/portals/{id}");
    let tender_path = format!("{path}/tenders");
    let run_path = format!("{path}/runs");
    let refresh = format!("{path}/refresh");
    let portal = wait_for(&app, &path, |value| value["state"] == "serving").await;
    assert_eq!(portal["tenderCount"], 2);
    assert!(get_json(&app, "/portals").await.is_array());
    let first = get_json(&app, &tender_path).await;
    assert_eq!(first[0]["id"], "A");
    assert_eq!(first[0]["publicationDate"], Value::Null);
    assert!(first[0].get("referenceNumber").is_none());
    let files = first[0]["documents"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    let checklist = files
        .iter()
        .find(|file| file["fileName"] == "checklist.pdf")
        .unwrap();
    assert_eq!(
        checklist["sha256"],
        format!("{:x}", Sha256::digest(CHECKLIST))
    );
    assert_eq!(
        call(&app, "GET", checklist["path"].as_str().unwrap(), None).await,
        (StatusCode::OK, CHECKLIST.to_vec()),
    );
    let guide = files
        .iter()
        .find(|file| file["fileName"] == "guide.pdf")
        .unwrap();
    assert_eq!(guide["sha256"], format!("{:x}", Sha256::digest(GUIDE)));
    assert_eq!(
        call(&app, "GET", guide["path"].as_str().unwrap(), None).await,
        (StatusCode::OK, GUIDE.to_vec()),
    );
    let document = files
        .iter()
        .find(|file| file["fileName"] == "spec.pdf")
        .unwrap();
    assert_eq!(
        document["sha256"],
        format!("{:x}", Sha256::digest(DOCUMENT))
    );
    assert_eq!(document["sizeBytes"], DOCUMENT.len());
    assert_eq!(document["fileName"], "spec.pdf");
    let file = document["path"].as_str().unwrap();
    assert_eq!(
        call(&app, "GET", file, None).await,
        (StatusCode::OK, DOCUMENT.to_vec())
    );
    let since = first[0]["lastChangedAt"]
        .as_str()
        .unwrap()
        .replace('+', "%2B");
    assert_eq!(
        get_json(&app, &format!("{tender_path}?changedSince={since}")).await,
        json!([])
    );

    let (a, b) = tokio::join!(
        call(&app, "POST", &refresh, None),
        call(&app, "POST", &refresh, None)
    );
    assert_eq!(a, b);
    assert_eq!(a.0, StatusCode::ACCEPTED);
    let run: Value = serde_json::from_slice(&a.1).unwrap();
    wait_for(&app, &run_path, |value| {
        value
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == run["runId"] && row["result"] == "ok")
    })
    .await;
    assert_eq!(get_json(&app, &tender_path).await, first);
    assert_eq!(model.learns.load(Ordering::SeqCst), 1);
    assert_eq!(model.verifies.load(Ordering::SeqCst), 1);

    fixture.layout.store(1, Ordering::SeqCst);
    assert_eq!(
        call(&app, "POST", &refresh, None).await.0,
        StatusCode::ACCEPTED
    );
    tokio::time::timeout(Duration::from_secs(10), model.repairing.notified())
        .await
        .unwrap();
    assert_eq!(get_json(&app, &path).await["state"], "healing");
    assert_eq!(get_json(&app, &tender_path).await, first);
    assert_eq!(call(&app, "GET", file, None).await.0, StatusCode::OK);
    let (a, b) = tokio::join!(
        call(&app, "POST", &refresh, None),
        call(&app, "POST", &refresh, None)
    );
    assert_eq!(a, b);
    assert_eq!(a.0, StatusCode::ACCEPTED);
    let queued: Value = serde_json::from_slice(&a.1).unwrap();
    assert!(
        !get_json(&app, &run_path)
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["id"] == queued["runId"])
    );
    model.release.notify_one();
    wait_for(&app, &path, |value| value["state"] == "serving").await;
    wait_for(&app, &run_path, |history| {
        history
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["id"] == queued["runId"] && run["result"] == "ok")
    })
    .await;
    let changed = get_json(&app, &format!("{tender_path}?changedSince={since}")).await;
    assert_eq!(changed.as_array().unwrap().len(), 1);
    assert_eq!(changed[0]["title"], "Park works, updated");
    let runs = get_json(&app, &run_path).await;
    assert_eq!(runs[2]["result"], "failed");
    assert_eq!(runs[3]["kind"], "repair");
    assert_eq!(runs[3]["result"], "ok");

    fixture.layout.store(3, Ordering::SeqCst);
    call(&app, "POST", &refresh, None).await;
    let failed = wait_for(&app, &path, |value| value["state"] == "failed").await;
    assert!(failed["reason"].as_str().unwrap().contains("login"));

    fixture.layout.store(2, Ordering::SeqCst);
    call(&app, "POST", &refresh, None).await;
    tokio::time::timeout(Duration::from_secs(10), model.repairing.notified())
        .await
        .unwrap();
    assert_eq!(
        get_json(&app, &tender_path).await.as_array().unwrap().len(),
        2
    );
    let requests_before_delete = fixture.requests.lock().unwrap().len();
    assert_eq!(
        call(&app, "DELETE", &path, None).await.0,
        StatusCode::NO_CONTENT
    );
    model.release.notify_one();
    assert_eq!(
        call(&app, "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(call(&app, "GET", file, None).await.0, StatusCode::NOT_FOUND);
    stop.cancel();
    worker.await.unwrap();
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), requests_before_delete);
    assert!(
        requests
            .windows(2)
            .all(|pair| pair[1] - pair[0] >= Duration::from_millis(900))
    );
    server.abort();
}

#[tokio::test]
async fn restart_records_interrupted_read_and_reschedules_active_portal() {
    let data = TempDir::new().unwrap();
    let db = data.path().join("restart.sqlite");
    let store = Store::open(&db).await.unwrap();
    let portal = store.register("https://example.com/").await.unwrap();
    let job = store.claim().await.unwrap().unwrap();
    drop(store);
    let reopened = Store::open(&db).await.unwrap();
    reopened.recover(Recovery::default().recheck).await.unwrap();
    assert!(
        reopened.auto_refresh().await.unwrap(),
        "scheduled reads start on"
    );
    reopened.set_auto_refresh(false).await.unwrap();
    reopened.enqueue_due().await.unwrap();
    assert!(reopened.claim().await.unwrap().is_none());
    reopened.set_auto_refresh(true).await.unwrap();
    assert!(reopened.auto_refresh().await.unwrap());
    reopened.enqueue_due().await.unwrap();
    let runs = reopened.runs(&portal).await.unwrap().unwrap();
    assert_eq!(runs[0].result.as_deref(), Some("failed"));
    let next = reopened.claim().await.unwrap().unwrap();
    assert_eq!(next.portal_id, portal);
    assert_ne!(next.id, job.id);
    let generation = reopened.remove(&portal).await.unwrap().unwrap();
    assert_eq!(
        reopened.register("https://example.com/").await.unwrap(),
        portal
    );
    assert!(!reopened.active(&next).await.unwrap());
    assert_ne!(
        reopened.claim().await.unwrap().unwrap().generation,
        generation
    );
}

/// Mirrors the GOV.UK Crime Contract 2025 failure: a tender's HTML attachment points onward to
/// another publication whose attachment list includes an external application.
#[tokio::test]
async fn fallback_pages_skip_external_tools_but_recipe_dead_ends_still_fail() {
    use reqwest::Url;
    const TERMS: &[u8] = b"%PDF-1.4\nStandard terms\n%%EOF\n";

    let tool_hits = Arc::new(AtomicUsize::new(0));
    let tally = tool_hits.clone();
    let tool = Router::new().route(
        "/ords/f",
        get(move || async move {
            tally.fetch_add(1, Ordering::SeqCst);
            Html(r#"<main><form><label>Postcode <input name="postcode"></label></form></main>"#)
        }),
    );
    let tool_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tool_url = format!(
        "http://{}/ords/f?p=109:45",
        tool_listener.local_addr().unwrap()
    );
    let tool_server = tokio::spawn(async move { axum::serve(tool_listener, tool).await });

    let attachment_list = format!(
        r#"<main><section class="attachment"><a href="/files/terms.pdf">Supervisor terms</a></section>
        <section class="attachment"><a href="/files/unrelated.pdf">Finance annex</a></section>
        <section class="attachment"><a href="{tool_url}">Police Station Register</a></section>
        <a href="/guidance/other">Other guidance</a></main>"#
    );
    let direct_tool = format!(
        r#"<article class="tender" data-id="T"><h2><a href="/tenders/t">Rota</a></h2>
        <section class="attachment"><a href="{tool_url}">Register</a></section></article>"#
    );
    let portal = Router::new()
        .route("/", get(|| async {
            Html(r#"<article class="tender" data-id="A"><h2>Crime contract</h2>
                <section class="attachment"><a href="/files/spec.pdf">Specification</a></section>
                <section class="attachment"><a href="/tender/supervisor">Supervisor forms</a></section>
                </article>"#)
        }))
        .route("/direct", get(move || async move { Html(direct_tool) }))
        .route("/tender/supervisor", get(|| async {
            Html(r#"<main><a href="/licence">Licence</a>
                <a href="/standard-contract">Supervisor Forms and Guidance</a></main>"#)
        }))
        .route("/standard-contract", get(move || async move { Html(attachment_list) }))
        .route("/files/spec.pdf", get(|| async {
            ([(header::CONTENT_TYPE, "application/pdf")], DOCUMENT)
        }))
        .route("/files/terms.pdf", get(|| async {
            ([(header::CONTENT_TYPE, "application/pdf")], TERMS)
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, portal).await });

    let dir = TempDir::new().unwrap();
    let fetcher = Fetcher::new(dir.path().into()).unwrap();
    let recipe: Recipe = serde_json::from_value(json!({
        "listing": "html", "items": ".tender",
        "list_fields": {"id": {"path": "", "attribute": "data-id"}, "title": {"path": "h2"},
            "url": {"path": "h2 a", "attribute": "href"}},
        "documents": {"path": ".attachment a", "attribute": "href"}
    }))
    .unwrap();

    let read = watchtower::recipe::read(&fetcher.portal(&base), &base, &recipe)
        .await
        .unwrap();
    let names: Vec<_> = read.tenders[0]
        .documents
        .iter()
        .map(|document| (document.file_name.as_str(), document.sha256.clone()))
        .collect();
    assert_eq!(
        names,
        [
            ("spec.pdf", format!("{:x}", Sha256::digest(DOCUMENT))),
            ("terms.pdf", format!("{:x}", Sha256::digest(TERMS))),
        ]
    );
    assert_eq!(tool_hits.load(Ordering::SeqCst), 0);

    let direct = base.join("direct").unwrap();
    let failure = watchtower::recipe::read(&fetcher.portal(&direct), &direct, &recipe)
        .await
        .unwrap_err();
    assert!(matches!(failure, ReadFailure::Incomplete(_)));
    let reason = failure.to_string();
    assert!(
        reason.contains("No attachment links matched on document page 127.0.0.1/ords/f"),
        "{reason}"
    );
    assert!(
        reason.contains(", linked from 127.0.0.1/tenders/t"),
        "{reason}"
    );
    assert!(!reason.contains("109:45"), "{reason}");
    assert_eq!(tool_hits.load(Ordering::SeqCst), 1);
    server.abort();
    tool_server.abort();
}

/// Mirrors the GOV.UK listing where one tender is a guidance page whose attachments use h2
/// headings while publication pages use h3.
#[tokio::test]
async fn detail_pages_with_another_template_use_the_html_fallback() {
    use reqwest::Url;
    const SECOND: &[u8] = b"%PDF-1.4\nGuidance template attachment\n%%EOF\n";

    let portal = Router::new()
        .route("/", get(|| async {
            Html(r#"<ul><li class="row" data-id="P"><a href="/publications/p">Publication</a></li>
                <li class="row" data-id="G"><a href="/guidance/g">Guidance</a></li>
                <li class="row" data-id="N"><a href="/guidance/none">No documents</a></li></ul>"#)
        }))
        .route("/publications/p", get(|| async {
            Html(r#"<main><h1>Publication</h1><section class="attachment">
                <h3 class="title"><a href="/files/first.pdf">First</a></h3></section></main>"#)
        }))
        .route("/guidance/g", get(|| async {
            Html(r#"<main><h1>Guidance</h1><a href="mailto:help@example.org">Help</a>
                <section class="attachment"><h2 class="title"><a href="/files/second.pdf">Second</a></h2></section>
                <a href="/web/login.shtml">e-Tendering system</a></main>"#)
        }))
        .route("/guidance/none", get(|| async {
            Html(r#"<main><h1>No documents yet</h1><a href="/contact">Contact</a></main>"#)
        }))
        .route("/files/first.pdf", get(|| async {
            ([(header::CONTENT_TYPE, "application/pdf")], DOCUMENT)
        }))
        .route("/files/second.pdf", get(|| async {
            ([(header::CONTENT_TYPE, "application/pdf")], SECOND)
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, portal).await });

    let dir = TempDir::new().unwrap();
    let fetcher = Fetcher::new(dir.path().into()).unwrap();
    let recipe: Recipe = serde_json::from_value(json!({
        "listing": "html", "items": ".row",
        "detail_link": {"path": "a", "attribute": "href"},
        "list_fields": {"id": {"path": "", "attribute": "data-id"}},
        "detail_fields": {"title": {"path": "h1"}},
        "documents": {"path": "h3.title a", "attribute": "href"}
    }))
    .unwrap();
    let read = watchtower::recipe::read(&fetcher.portal(&base), &base, &recipe)
        .await
        .unwrap();
    let documents: Vec<_> = read
        .tenders
        .iter()
        .map(|tender| {
            (
                tender.id.clone().unwrap(),
                tender
                    .documents
                    .iter()
                    .map(|document| (document.file_name.clone(), document.sha256.clone()))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    assert_eq!(
        documents,
        [
            (
                "P".to_owned(),
                vec![(
                    "first.pdf".to_owned(),
                    format!("{:x}", Sha256::digest(DOCUMENT))
                )]
            ),
            (
                "G".to_owned(),
                vec![(
                    "second.pdf".to_owned(),
                    format!("{:x}", Sha256::digest(SECOND))
                )]
            ),
            ("N".to_owned(), vec![]),
        ]
    );
    server.abort();
}

#[tokio::test]
async fn public_json_explicit_empty_and_invalid_recipe() {
    use reqwest::Url;
    let app = Router::new().route(
        "/",
        get(|| async { Json(json!({"items": [], "total": 0})) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let dir = TempDir::new().unwrap();
    let fetcher = Fetcher::new(dir.path().into()).unwrap();
    let recipe: Recipe = serde_json::from_value(json!({
        "listing":"json", "items":"/items", "total":{"path":"/total"}
    }))
    .unwrap();
    let result = watchtower::recipe::read(&fetcher.portal(&url), &url, &recipe)
        .await
        .unwrap();
    assert!(result.evidence.terminal_page);
    assert!(result.tenders.is_empty());
    let bad: Recipe = serde_json::from_value(json!({"listing":"html", "items":"["})).unwrap();
    assert!(bad.validate().is_err());
    server.abort();
}

#[tokio::test]
async fn connection_returns_latest_decision_null_before_one_and_404_for_unknown_portals() {
    let data = TempDir::new().unwrap();
    let store = Store::open(&data.path().join("connection.sqlite"))
        .await
        .unwrap();
    let app = api::router(Api {
        store: store.clone(),
        jobs: Jobs::default(),
        files: Arc::new(data.path().join("files")),
    });
    let (status, _) = call(&app, "GET", "/portals/unknown/connection", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let portal = store.register("https://example.com/").await.unwrap();
    let path = format!("/portals/{portal}/connection");
    assert_eq!(get_json(&app, &path).await, Value::Null);

    let job = store.claim().await.unwrap().unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/connection_decision.json")).unwrap();
    let mut first: ConnectionDecision = serde_json::from_value(fixture.clone()).unwrap();
    first.attempt = 1;
    first.chosen.method = watchtower::domain::ConnectionMethod::Html;
    store.record_decision(&job, &first).await.unwrap();
    let second: ConnectionDecision = serde_json::from_value(fixture.clone()).unwrap();
    store.record_decision(&job, &second).await.unwrap();
    let latest = get_json(&app, &path).await;
    assert_eq!(
        latest, fixture,
        "the latest attempt, in the shared camelCase shape"
    );
    assert_eq!(latest["chosen"]["method"], "ocds");

    store.remove(&portal).await.unwrap();
    let (status, _) = call(&app, "GET", &path, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        store.register("https://example.com/").await.unwrap(),
        portal
    );
    assert_eq!(
        get_json(&app, &path).await,
        Value::Null,
        "a new registration starts without the removed decision"
    );
}
