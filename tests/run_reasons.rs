//! Run reasons: a read that broke records what broke and the evidence, the repair that fixed it
//! records what it changed and why, and the dashboard shows both together. The appendix route
//! `GET /portals/{id}/runs` keeps its shape without reasons.

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode, header},
    response::Html,
    routing::get as get_route,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
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

/// Learns the `.tender` layout on attach and the `.offer` layout on repair.
struct Relearner;

#[async_trait::async_trait]
impl PortalModel for Relearner {
    async fn learn(
        &self,
        _: &ReasoningContext<'_>,
        previous: Option<(&Recipe, &str)>,
        _: &ConnectionDecision,
    ) -> Result<Recipe, ReadFailure> {
        let items = if previous.is_some() {
            ".offer"
        } else {
            ".tender"
        };
        Ok(serde_json::from_value(json!({
            "listing": "html", "items": items, "total": {"path": "#total"},
            "list_fields": {"id": {"path": "", "attribute": "data-id"}, "title": {"path": "h2"},
                "url": {"path": ".detail", "attribute": "href"}}
        }))
        .unwrap())
    }

    async fn verify(
        &self,
        _: &ReasoningContext<'_>,
        _: &Recipe,
        _: &CandidateRead,
        _: &Review,
    ) -> Result<(), ReadFailure> {
        Ok(())
    }
}

async fn get(app: &Router, path: &str) -> (StatusCode, String) {
    let request = Request::builder().uri(path).body(Body::empty()).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

async fn get_json(app: &Router, path: &str) -> Value {
    let (status, body) = get(app, path).await;
    assert_eq!(status, StatusCode::OK, "{path}");
    serde_json::from_str(&body).unwrap()
}

async fn wait_for(app: &Router, path: &str, matches: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(20), async {
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

#[tokio::test]
async fn failed_read_and_its_repair_record_reasons_the_dashboard_shows_together() {
    let layout = Arc::new(AtomicU8::new(0));
    let site = Router::new()
        .route(
            "/",
            get_route(|State(layout): State<Arc<AtomicU8>>| async move {
                let class = if layout.load(Ordering::SeqCst) == 0 {
                    "tender"
                } else {
                    "offer"
                };
                Html(format!(
                    r#"<b id="total">2</b>
                    <article class="{class}" data-id="A"><h2>Road survey</h2><a class="detail" href="/a">Detail</a></article>
                    <article class="{class}" data-id="B"><h2>Park works</h2><a class="detail" href="/b">Detail</a></article>"#
                ))
            }),
        )
        .with_state(layout.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, site).await });

    let data = TempDir::new().unwrap();
    let files = Arc::new(data.path().join("files"));
    tokio::fs::create_dir(&*files).await.unwrap();
    let store = Store::open(&data.path().join("test.sqlite")).await.unwrap();
    let jobs = Jobs::default();
    let stop = CancellationToken::new();
    let worker = tokio::spawn(
        Worker {
            store: store.clone(),
            jobs: jobs.clone(),
            fetcher: Fetcher::new(files.as_ref().clone()).unwrap(),
            model: Arc::new(Relearner),
            interval: 900,
            recovery: Recovery::default(),
        }
        .run(stop.clone()),
    );
    let app = api::router(Api { store, jobs, files });

    let request = Request::builder()
        .method("POST")
        .uri("/portals")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "url": url }).to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    let id = serde_json::from_slice::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/portals/{id}");
    let runs_path = format!("{path}/runs");
    wait_for(&app, &path, |portal| portal["state"] == "serving").await;

    // The redesign answers HTTP 200 but no longer matches the learned row selector.
    layout.store(1, Ordering::SeqCst);
    let request = Request::builder()
        .method("POST")
        .uri(format!("{path}/refresh"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    let runs = wait_for(&app, &runs_path, |runs| {
        let runs = runs.as_array().unwrap();
        runs.len() == 3 && runs.iter().all(|run| !run["result"].is_null())
    })
    .await;
    let kinds: Vec<_> = runs
        .as_array()
        .unwrap()
        .iter()
        .map(|run| {
            (
                run["kind"].as_str().unwrap(),
                run["result"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [("read", "ok"), ("read", "failed"), ("repair", "ok")]
    );
    let portal = get_json(&app, &path).await;
    assert_eq!(portal["state"], "serving");
    assert_eq!(portal["reason"], "Last read verified");

    // The appendix shape stays exactly as specified.
    for run in runs.as_array().unwrap() {
        let mut keys: Vec<_> = run.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["finishedAt", "id", "kind", "result", "startedAt"]);
    }

    let (status, history) = get(&app, &format!("/dashboard{path}/history")).await;
    assert_eq!(status, StatusCode::OK);
    let pair = history
        .find(r#"<li class="run-pair">"#)
        .expect("the broken read and its repair share one entry");
    let broke = history
        .find("read · failed · what broke")
        .expect("the failed read is labelled");
    let fixed = history
        .find("repair · ok · how it was repaired")
        .expect("the repair is labelled");
    assert!(pair < broke && broke < fixed, "{history}");
    let reasons: Vec<&str> = history
        .split(r#"<p class="run-reason">"#)
        .skip(1)
        .map(|rest| rest.split("</p>").next().unwrap())
        .collect();
    // Newest first: the pair (read, then repair), then the attach.
    assert_eq!(reasons.len(), 3, "{history}");
    let (read, repair, attach) = (reasons[0], reasons[1], reasons[2]);
    assert!(
        read.contains("The last verified read held 2 tender(s), 2 open, and stays served"),
        "{read}"
    );
    assert!(
        repair.starts_with(
            "Repaired via HTML: changed items &quot;.tender&quot; → &quot;.offer&quot;. Fixes: "
        ),
        "{repair}"
    );
    let failure = read.split(". The last verified read").next().unwrap();
    assert!(!failure.is_empty());
    assert!(repair.contains(failure), "the repair names what it fixed");
    assert!(repair.ends_with("2 tender(s) held, 0 changed"), "{repair}");
    assert_eq!(attach, "Attached and verified. 2 tender(s) held, 2 changed");

    assert_eq!(
        get(&app, "/dashboard/portals/unknown/history").await.0,
        StatusCode::NOT_FOUND
    );
    stop.cancel();
    worker.await.unwrap();
    server.abort();
}
