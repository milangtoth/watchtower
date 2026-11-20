//! A local portal whose public listing and details can change layout during a live test.
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse},
    routing::{get, post},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default)]
struct Portal(Arc<AtomicBool>);

impl Portal {
    fn changed(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

const TENDERS: [(&str, &str, &str); 2] = [
    (
        "PARK-01",
        "Park lighting",
        "Replace the path lighting in the municipal park.",
    ),
    (
        "WATER-02",
        "Water quality survey",
        "Survey water quality at three municipal sites.",
    ),
];

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let address = std::env::var("MOCK_PORTAL_ADDR").unwrap_or_else(|_| "127.0.0.1:8091".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    let app = Router::new()
        .route("/", get(listing))
        .route("/tenders/{id}", get(detail))
        .route("/opportunities/{id}", get(detail))
        .route("/documents/{filename}", get(document))
        .route("/__fixture/change-layout", post(change_layout))
        .with_state(Portal::default());
    println!("Mock portal listening at http://{address}/");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn change_layout(State(portal): State<Portal>) -> StatusCode {
    portal.0.store(true, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

async fn listing(State(portal): State<Portal>) -> Html<String> {
    let changed = portal.changed();
    let rows = TENDERS.iter().map(|(id, title, _)| {
        if changed {
            format!(r#"<tr data-reference="{id}"><td>{id}</td><td><a href="/opportunities/{id}">{title}</a></td></tr>"#)
        } else {
            format!(r#"<article class="tender-card" data-id="{id}"><h2><a href="/tenders/{id}">{title}</a></h2></article>"#)
        }
    }).collect::<String>();
    let body = if changed {
        format!(
            "<table id=\"opportunities\"><thead><tr><th>Reference</th><th>Opportunity</th></tr></thead><tbody>{rows}</tbody></table>"
        )
    } else {
        format!("<section id=\"open-tenders\">{rows}</section>")
    };
    Html(format!(
        "<!doctype html><html lang=\"en\"><title>Municipal procurement</title><main><h1>Open tenders</h1><p>2 opportunities</p>{body}</main></html>"
    ))
}

async fn detail(State(portal): State<Portal>, Path(id): Path<String>) -> impl IntoResponse {
    let Some((_, title, specification)) = TENDERS.iter().find(|(code, _, _)| *code == id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let changed = portal.changed();
    let title = if changed && id == "PARK-01" {
        "Park lighting — amended specification"
    } else {
        title
    };
    let version = if changed { "v2" } else { "v1" };
    let metadata = if changed {
        format!(
            r#"<dl><dt>Reference</dt><dd class="ref">{id}</dd>
          <dt>Contracting authority</dt><dd class="authority">Riverton Municipality</dd>
          <dt>Published</dt><dd><time class="published" datetime="2026-11-20">20 November 2026</time></dd>
          <dt>Submission deadline</dt><dd><time class="closing" datetime="2099-02-12T17:00:00+01:00">12 February 2099, 17:00 CET</time></dd></dl>"#
        )
    } else {
        format!(
            r#"<p class="tender-id">{id}</p><p class="buyer">Riverton Municipality</p>
          <time class="publication" datetime="2026-11-20">20 November 2026</time>
          <p>Submission deadline <time class="deadline" datetime="2099-02-12T17:00:00+01:00">12 February 2099, 17:00 CET</time></p>"#
        )
    };
    Html(format!(r#"<!doctype html><html lang="en"><title>{title}</title><main>
      <h1>{title}</h1>{metadata}<p>{specification}</p>
      <section id="attachments"><h2>Documents</h2><a download href="/documents/{id}-{version}.txt">Tender specification</a></section>
      </main></html>"#)).into_response()
}

async fn document(Path(name): Path<String>) -> impl IntoResponse {
    for (id, _, specification) in TENDERS {
        for (version, note) in [("v1", "Original issue"), ("v2", "Amended issue")] {
            if name == format!("{id}-{version}.txt") {
                return (
                    [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                    format!("{id} — {note}\n{specification}\n"),
                )
                    .into_response();
            }
        }
    }
    StatusCode::NOT_FOUND.into_response()
}
