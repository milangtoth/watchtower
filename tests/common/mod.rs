//! Fixture portal, model stand-in and service harness shared by the integration tests.
#![allow(dead_code)]

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, State},
    http::{Request, StatusCode, header},
    response::{Html, IntoResponse},
    routing::get,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use watchtower::{
    api::{self, Api},
    db::Store,
    domain::{
        CandidateRead, ConnectionCandidate, ConnectionDecision, ConnectionMethod, ReadFailure,
        Review,
    },
    fetch::Fetcher,
    model::{PortalModel, ReasoningContext, top_ranked},
    recipe::Recipe,
    worker::{Jobs, Recovery, Worker},
};

pub const DEADLINE: &str = "2099-12-09T10:00:00+01:00";

#[derive(Clone, Debug)]
pub struct Row {
    pub id: &'static str,
    pub title: &'static str,
    pub buyer: &'static str,
    pub files: Vec<&'static str>,
}

pub fn alpha() -> Row {
    Row {
        id: "A",
        title: "Road survey",
        buyer: "City of Alpha",
        files: vec!["a.pdf"],
    }
}

pub fn beta() -> Row {
    Row {
        id: "B",
        title: "Park works",
        buyer: "City of Beta",
        files: vec!["b.pdf"],
    }
}

/// What the fixture portal shows. `pages` holds the source truth. The other fields change only
/// the markup, as a redesign that still answers HTTP 200 would.
#[derive(Clone, Debug)]
pub struct Layout {
    pub pages: Vec<Vec<Row>>,
    /// Text of `#total`. `None` removes the element.
    pub total: Option<String>,
    /// Class of the link to the next listing page.
    pub next_class: &'static str,
    /// Inserts a lot column between title and buyer.
    pub lot_column: bool,
    /// Renders each row with the next row's file links.
    pub shift_documents: bool,
    /// Shows the portal's own "no open tenders" message.
    pub empty_marker: bool,
    /// The JSON listing answers with an empty array and nothing else.
    pub json_empty: bool,
    /// Listing pages answer with this HTTP status and optional `Retry-After` value.
    pub listing_status: Option<(u16, Option<&'static str>)>,
    /// Listing pages answer 401 with a sign-in page.
    pub login_wall: bool,
    /// When the first listing page was requested.
    pub listing_hits: Vec<std::time::Instant>,
    /// The OCDS endpoint answers with something that is not a package.
    pub ocds_broken: bool,
    /// The OCDS endpoint answers pages after the first with HTML, as a throttled API might.
    pub ocds_throttled: bool,
    /// Listing pages also carry "First" and "Previous" links, and the last page a link to
    /// itself, all with the next-link class.
    pub nav_links: bool,
    /// OCDS release dates (RFC 3339) by tender ID, in place of the fixture's default date.
    pub ocds_dates: BTreeMap<&'static str, String>,
    /// OCDS tender statuses by tender ID, in place of `active`.
    pub ocds_statuses: BTreeMap<&'static str, &'static str>,
}

impl Layout {
    /// A on page 1, B only on page 2, with a matching total.
    pub fn two_pages() -> Self {
        Self {
            pages: vec![vec![alpha()], vec![beta()]],
            total: Some("2".into()),
            next_class: "next",
            lot_column: false,
            shift_documents: false,
            empty_marker: false,
            json_empty: false,
            listing_status: None,
            login_wall: false,
            listing_hits: Vec::new(),
            ocds_broken: false,
            ocds_throttled: false,
            nav_links: false,
            ocds_dates: BTreeMap::new(),
            ocds_statuses: BTreeMap::new(),
        }
    }

    pub fn one_page() -> Self {
        Self {
            pages: vec![vec![alpha(), beta()]],
            ..Self::two_pages()
        }
    }

    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.pages.iter().flatten()
    }
}

pub type Site = Arc<Mutex<Layout>>;

pub fn file_bytes(name: &str) -> Vec<u8> {
    format!("%PDF-1.4\nFixture file {name}\n%%EOF\n").into_bytes()
}

pub fn sha(name: &str) -> String {
    format!("{:x}", Sha256::digest(file_bytes(name)))
}

pub fn render(layout: &Layout, index: usize) -> String {
    let Some(rows) = layout.pages.get(index) else {
        return "<main><p>Page not found</p></main>".into();
    };
    let mut files: Vec<_> = rows.iter().map(|row| row.files.clone()).collect();
    if layout.shift_documents && !files.is_empty() {
        files.rotate_left(1);
    }
    let mut html = String::from("<main><h1>Open tenders</h1>");
    if let Some(total) = &layout.total {
        html += &format!(r#"<p>Results: <b id="total">{total}</b></p>"#);
    }
    if layout.empty_marker {
        html += r#"<p class="no-results">No open tenders</p>"#;
    }
    html += "<table>";
    for (position, (row, files)) in rows.iter().zip(files).enumerate() {
        let lot = if layout.lot_column {
            format!("<td>Lot {}</td>", position + 1)
        } else {
            String::new()
        };
        let links: String = files
            .iter()
            .map(|name| format!(r#" <a class="file" href="/files/{name}">{name}</a>"#))
            .collect();
        html += &format!(
            r#"<tr class="tender" data-id="{id}"><td>{title}</td>{lot}<td>{buyer}</td><td><time datetime="{DEADLINE}">9 December 2099</time></td><td><a class="detail" href="/tenders/{id}">Notice</a>{links}</td></tr>"#,
            id = row.id,
            title = row.title,
            buyer = row.buyer,
        );
    }
    html += "</table>";
    let page_path = |page: usize| {
        if page == 0 {
            "/".to_owned()
        } else {
            format!("/page/{page}")
        }
    };
    html += "<nav>";
    if layout.nav_links && index > 0 {
        html += &format!(
            r#"<a class="{class}" href="/">First</a><a class="{class}" href="{previous}">Previous</a>"#,
            class = layout.next_class,
            previous = page_path(index - 1)
        );
    }
    if index + 1 < layout.pages.len() {
        html += &format!(
            r#"<a class="{}" href="/page/{}">Next page</a>"#,
            layout.next_class,
            index + 1
        );
    } else if layout.nav_links {
        html += &format!(
            r#"<a class="{}" href="{}">Next page</a>"#,
            layout.next_class,
            page_path(index)
        );
    }
    html + "</nav></main>"
}

fn listing(site: &Site, index: usize) -> axum::response::Response {
    let mut layout = site.lock().unwrap();
    if index == 0 {
        layout.listing_hits.push(std::time::Instant::now());
    }
    if layout.login_wall {
        return (
            StatusCode::UNAUTHORIZED,
            Html("<title>Sign in</title><form><input type=\"password\"></form>"),
        )
            .into_response();
    }
    if let Some((status, retry_after)) = layout.listing_status {
        let mut response = StatusCode::from_u16(status).unwrap().into_response();
        if let Some(value) = retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, value.parse().unwrap());
        }
        return response;
    }
    Html(render(&layout, index)).into_response()
}

/// A tender's detail page, linked from the feed and the sitemap.
pub fn detail_page(row: &Row) -> String {
    let links: String = row
        .files
        .iter()
        .map(|name| format!(r#"<a class="file" href="/files/{name}">{name}</a>"#))
        .collect();
    format!(
        r#"<main data-id="{id}"><h1>{title}</h1><p>Buyer: <span class="buyer">{buyer}</span></p><time datetime="{DEADLINE}">9 December 2099</time>{links}</main>"#,
        id = row.id,
        title = row.title,
        buyer = row.buyer,
    )
}

/// One page of an OCDS release package, following the listing's own pages.
pub fn ocds_page(layout: &Layout, index: usize) -> Value {
    if layout.ocds_broken {
        return json!({"error": "maintenance"});
    }
    let releases: Vec<Value> = layout
        .pages
        .get(index)
        .into_iter()
        .flatten()
        .map(|row| ocds_release(layout, row))
        .collect();
    let mut package = json!({"releases": releases});
    if index + 1 < layout.pages.len() {
        package["links"] = json!({"next": format!("/ocds/{}", index + 1)});
    }
    package
}

/// The releases of every listing page dated on or after `from`, as an API filtered by date
/// answers.
pub fn ocds_since(layout: &Layout, from: &str) -> Value {
    let from = chrono::NaiveDateTime::parse_from_str(from, "%Y-%m-%dT%H:%M:%S")
        .unwrap()
        .and_utc();
    let releases: Vec<Value> = layout
        .rows()
        .map(|row| ocds_release(layout, row))
        .filter(|release| {
            chrono::DateTime::parse_from_rfc3339(release["date"].as_str().unwrap()).unwrap() >= from
        })
        .collect();
    json!({"releases": releases})
}

fn ocds_release(layout: &Layout, row: &Row) -> Value {
    let date = layout
        .ocds_dates
        .get(row.id)
        .map_or("2026-11-01T09:00:00+01:00", String::as_str);
    let status = layout
        .ocds_statuses
        .get(row.id)
        .copied()
        .unwrap_or("active");
    json!({
        "ocid": format!("ocds-fixture-{}", row.id), "date": date,
        "buyer": {"name": row.buyer},
        "tender": {"id": row.id, "title": row.title, "status": status,
            "procurementMethod": "open", "mainProcurementCategory": "services",
            "tenderPeriod": {"endDate": DEADLINE},
            "items": [{"classification": {"scheme": "CPV", "id": "72000000-5"}}],
            "documents": row.files.iter().map(|name| json!({"url": format!("/files/{name}")})).collect::<Vec<_>>()}
    })
}

pub async fn serve(site: Site) -> (String, tokio::task::JoinHandle<std::io::Result<()>>) {
    let router = Router::new()
        .route(
            "/",
            get(|State(site): State<Site>| async move { listing(&site, 0) }),
        )
        .route(
            "/page/{index}",
            get(
                |State(site): State<Site>, Path(index): Path<usize>| async move {
                    listing(&site, index)
                },
            ),
        )
        .route(
            "/api",
            get(|State(site): State<Site>| async move {
                let layout = site.lock().unwrap().clone();
                let items: Vec<_> = if layout.json_empty {
                    Vec::new()
                } else {
                    layout
                        .rows()
                        .map(|row| {
                            json!({"id": row.id, "title": row.title, "buyer": row.buyer,
                                "deadline": DEADLINE, "url": format!("/tenders/{}", row.id),
                                "files": row.files.iter().map(|name| format!("/files/{name}")).collect::<Vec<_>>()})
                        })
                        .collect()
                };
                Json(json!({ "items": items }))
            }),
        )
        .route(
            "/tenders/{id}",
            get(
                |State(site): State<Site>, Path(id): Path<String>| async move {
                    let layout = site.lock().unwrap();
                    match layout.rows().find(|row| row.id == id) {
                        Some(row) => Html(detail_page(row)).into_response(),
                        None => StatusCode::NOT_FOUND.into_response(),
                    }
                },
            ),
        )
        .route(
            "/feed.xml",
            get(|State(site): State<Site>| async move {
                let layout = site.lock().unwrap();
                let items: String = layout
                    .rows()
                    .map(|row| {
                        format!(
                            "<item><title><![CDATA[{}]]></title><link>/tenders/{}</link></item>",
                            row.title, row.id
                        )
                    })
                    .collect();
                (
                    [(header::CONTENT_TYPE, "application/rss+xml")],
                    format!(
                        r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Fixture</title><link>/</link>{items}</channel></rss>"#
                    ),
                )
            }),
        )
        .route(
            "/sitemap.xml",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "application/xml")],
                    r#"<?xml version="1.0"?><sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><sitemap><loc>/sitemap-tenders.xml.gz</loc></sitemap></sitemapindex>"#,
                )
            }),
        )
        .route(
            "/sitemap-tenders.xml.gz",
            get(|State(site): State<Site>| async move {
                let layout = site.lock().unwrap();
                let urls: String = layout
                    .rows()
                    .map(|row| format!("<url><loc>/tenders/{}</loc></url>", row.id))
                    .collect();
                let xml = format!(
                    r#"<?xml version="1.0"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9"><url><loc>/about</loc></url>{urls}</urlset>"#
                );
                let mut gzip =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                std::io::Write::write_all(&mut gzip, xml.as_bytes()).unwrap();
                ([(header::CONTENT_TYPE, "application/gzip")], gzip.finish().unwrap())
            }),
        )
        .route(
            "/ocds",
            get(|State(site): State<Site>| async move { Json(ocds_page(&site.lock().unwrap(), 0)) }),
        )
        .route(
            "/ocds/since",
            get(
                |State(site): State<Site>,
                 axum::extract::Query(query): axum::extract::Query<
                    std::collections::HashMap<String, String>,
                >| async move {
                    Json(ocds_since(&site.lock().unwrap(), &query["updatedFrom"]))
                },
            ),
        )
        .route(
            "/ocds/{index}",
            get(
                |State(site): State<Site>, Path(index): Path<usize>| async move {
                    let layout = site.lock().unwrap();
                    if layout.ocds_throttled {
                        return Html("<html><body>Too many requests, try later</body></html>")
                            .into_response();
                    }
                    Json(ocds_page(&layout, index)).into_response()
                },
            ),
        )
        .route(
            "/api/paged",
            get(
                |State(site): State<Site>,
                 axum::extract::Query(query): axum::extract::Query<
                    std::collections::HashMap<String, String>,
                >| async move {
                    let layout = site.lock().unwrap();
                    let page: usize = query.get("page").and_then(|p| p.parse().ok()).unwrap_or(0);
                    let items: Vec<Value> = layout
                        .pages
                        .get(page)
                        .into_iter()
                        .flatten()
                        .map(|row| {
                            json!({"id": row.id, "title": row.title, "buyer": row.buyer,
                                "deadline": DEADLINE, "url": format!("/tenders/{}", row.id),
                                "files": row.files.iter().map(|name| format!("/files/{name}")).collect::<Vec<_>>()})
                        })
                        .collect();
                    Json(json!({"content": items, "totalElements": layout.rows().count(),
                        "number": page, "last": page + 1 >= layout.pages.len()}))
                },
            ),
        )
        .route(
            "/files/{name}",
            get(|Path(name): Path<String>| async move {
                ([(header::CONTENT_TYPE, "application/pdf")], file_bytes(&name))
            }),
        )
        .with_state(site);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move { axum::serve(listener, router).await }),
    )
}

/// The recipe a model might learn for the HTML fixture. Tests drop `total` or `next` to stand
/// for a portal without a total or a model that missed pagination.
pub fn html_recipe() -> Value {
    json!({
        "listing": "html", "items": "tr.tender",
        "total": {"path": "#total"},
        "next": {"path": "a.next", "attribute": "href"},
        "empty": {"path": ".no-results"},
        "list_fields": {
            "id": {"path": "", "attribute": "data-id"},
            "title": {"path": "td:nth-child(1)"},
            "buyerName": {"path": "td:nth-child(2)"},
            "submissionDeadline": {"path": "time", "attribute": "datetime"},
            "url": {"path": "a.detail", "attribute": "href"}
        },
        "direct_documents": {"path": "a.file", "attribute": "href"}
    })
}

pub fn without(mut recipe: Value, keys: &[&str]) -> Value {
    for key in keys {
        recipe.as_object_mut().unwrap().remove(*key);
    }
    recipe
}

pub struct Auditor {
    pub site: Site,
    pub recipe: Value,
    /// Recipes learned for particular connection methods instead of `recipe`.
    pub recipes: Vec<(ConnectionMethod, Value)>,
    pub learns: AtomicUsize,
    pub verifies: AtomicUsize,
    pub decisions: AtomicUsize,
    /// Connection methods `decide` returns in order, with paths on the fixture. Empty keeps the
    /// default decision for the probed candidates.
    pub connections: Vec<(ConnectionMethod, &'static str)>,
    pub fakes: Fakes,
}

/// Model and probe behaviour a test can add to the stand-in.
#[derive(Default)]
pub struct Fakes {
    /// Connection methods learning declares unsupported, as the model does for an empty app
    /// shell.
    pub unsupported: Vec<ConnectionMethod>,
    /// Paths among `connections` whose probe sample showed no items and no pagination.
    pub bare: Vec<&'static str>,
}

#[async_trait::async_trait]
impl PortalModel for Auditor {
    async fn learn(
        &self,
        _: &ReasoningContext<'_>,
        _: Option<(&Recipe, &str)>,
        decision: &ConnectionDecision,
    ) -> Result<Recipe, ReadFailure> {
        self.learns.fetch_add(1, Ordering::SeqCst);
        if self.fakes.unsupported.contains(&decision.chosen.method) {
            return Err(ReadFailure::Unsupported(format!(
                "Learning stopped after 1 model call(s), method unsupported: {} entry page is an empty app shell",
                decision.chosen.method.label()
            )));
        }
        let recipe = self
            .recipes
            .iter()
            .find(|(method, _)| *method == decision.chosen.method)
            .map_or(&self.recipe, |(_, recipe)| recipe);
        Ok(serde_json::from_value(recipe.clone()).unwrap())
    }

    async fn decide(
        &self,
        ctx: &ReasoningContext<'_>,
        ranked: Vec<ConnectionCandidate>,
    ) -> Result<ConnectionDecision, ReadFailure> {
        self.decisions.fetch_add(1, Ordering::SeqCst);
        if self.connections.is_empty() {
            return top_ranked(ranked, 1);
        }
        let candidates = self
            .connections
            .iter()
            .map(|(method, path)| {
                let bare = self.fakes.bare.contains(path);
                ConnectionCandidate {
                    method: *method,
                    entry_url: ctx.url.join(path).unwrap().to_string(),
                    found_via: "test".into(),
                    http_status: 200,
                    content_type: None,
                    sample_items: if bare { 0 } else { 2 },
                    fields_seen: if bare {
                        Vec::new()
                    } else {
                        vec!["title".into()]
                    },
                    has_pagination: false,
                    score: 0.5,
                }
            })
            .collect();
        top_ranked(candidates, 1)
    }

    async fn redecide(
        &self,
        _: &ReasoningContext<'_>,
        ranked: Vec<ConnectionCandidate>,
    ) -> Result<ConnectionDecision, ReadFailure> {
        self.decisions.fetch_add(1, Ordering::SeqCst);
        top_ranked(ranked, 1)
    }

    async fn verify(
        &self,
        _: &ReasoningContext<'_>,
        _: &Recipe,
        candidate: &CandidateRead,
        review: &Review,
    ) -> Result<(), ReadFailure> {
        self.verifies.fetch_add(1, Ordering::SeqCst);
        if !review.unfollowed_links.is_empty() {
            return Err(ReadFailure::Incomplete(
                "Pagination links on the listing were not followed".into(),
            ));
        }
        let truth: BTreeMap<_, _> = {
            let layout = self.site.lock().unwrap();
            layout
                .rows()
                .map(|row| (row.id.to_owned(), row.clone()))
                .collect()
        };
        // Production verify shows the model at most four parsed tenders.
        for tender in candidate.tenders.iter().take(4) {
            let id = tender.id.clone().unwrap_or_default();
            let Some(row) = truth.get(&id) else {
                return Err(ReadFailure::Incomplete(format!(
                    "Sampled tender {id} is not on the source"
                )));
            };
            let mut files: Vec<_> = tender
                .documents
                .iter()
                .map(|d| d.file_name.as_str())
                .collect();
            files.sort_unstable();
            let mut expected = row.files.clone();
            expected.sort_unstable();
            if tender.title.as_deref() != Some(row.title)
                || tender.buyer_name.as_deref() != Some(row.buyer)
                || files != expected
            {
                return Err(ReadFailure::Incomplete(format!(
                    "Sampled tender {id} does not match its source row"
                )));
            }
        }
        Ok(())
    }
}

pub struct Harness {
    pub app: Router,
    pub store: Store,
    pub site: Site,
    pub model: Arc<Auditor>,
    pub portal: String,
    pub stop: CancellationToken,
    pub worker: tokio::task::JoinHandle<()>,
    pub server: tokio::task::JoinHandle<std::io::Result<()>>,
    pub _data: TempDir,
}

pub async fn call(
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
    let status = response.status();
    (
        status,
        to_bytes(response.into_body(), 10_000_000)
            .await
            .unwrap()
            .to_vec(),
    )
}

pub async fn get_json(app: &Router, path: &str) -> Value {
    let (status, bytes) = call(app, "GET", path, None).await;
    assert_eq!(status, StatusCode::OK, "{path}");
    serde_json::from_slice(&bytes).unwrap()
}

impl Harness {
    /// Starts the service on empty state, registers the fixture and waits for the first read
    /// and any repair to finish.
    pub async fn attach(layout: Layout, recipe: Value, path: &str) -> Self {
        Self::attach_with(layout, recipe, path, 900, Recovery::default()).await
    }

    /// Like `attach`, with a regular read interval and recovery policy in seconds.
    pub async fn attach_with(
        layout: Layout,
        recipe: Value,
        path: &str,
        interval: i64,
        recovery: Recovery,
    ) -> Self {
        Self::start(
            layout,
            recipe,
            path,
            interval,
            recovery,
            Vec::new(),
            Vec::new(),
            Fakes::default(),
        )
        .await
    }

    /// Like `attach`, with the connection methods the model decides on, in order.
    pub async fn attach_connected(
        layout: Layout,
        recipe: Value,
        connections: Vec<(ConnectionMethod, &'static str)>,
    ) -> Self {
        Self::start(
            layout,
            recipe,
            "/",
            900,
            Recovery::default(),
            connections,
            Vec::new(),
            Fakes::default(),
        )
        .await
    }

    /// Like `attach_connected`, with the recipe the model learns for each connection method.
    pub async fn attach_methods(
        layout: Layout,
        connections: Vec<(ConnectionMethod, &'static str)>,
        recipes: Vec<(ConnectionMethod, Value)>,
    ) -> Self {
        Self::attach_faked(layout, connections, recipes, Fakes::default()).await
    }

    /// Like `attach_methods`, with unsupported methods or bare probe samples.
    pub async fn attach_faked(
        layout: Layout,
        connections: Vec<(ConnectionMethod, &'static str)>,
        recipes: Vec<(ConnectionMethod, Value)>,
        fakes: Fakes,
    ) -> Self {
        Self::start(
            layout,
            html_recipe(),
            "/",
            900,
            Recovery::default(),
            connections,
            recipes,
            fakes,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn start(
        layout: Layout,
        recipe: Value,
        path: &str,
        interval: i64,
        recovery: Recovery,
        connections: Vec<(ConnectionMethod, &'static str)>,
        recipes: Vec<(ConnectionMethod, Value)>,
        fakes: Fakes,
    ) -> Self {
        let site: Site = Arc::new(Mutex::new(layout));
        let (base, server) = serve(site.clone()).await;
        let data = TempDir::new().unwrap();
        let files = Arc::new(data.path().join("files"));
        tokio::fs::create_dir(&*files).await.unwrap();
        let store = Store::open(&data.path().join("trust.sqlite"))
            .await
            .unwrap();
        let jobs = Jobs::default();
        let model = Arc::new(Auditor {
            site: site.clone(),
            recipe,
            recipes,
            learns: AtomicUsize::new(0),
            verifies: AtomicUsize::new(0),
            decisions: AtomicUsize::new(0),
            connections,
            fakes,
        });
        let stop = CancellationToken::new();
        let worker = tokio::spawn(
            Worker {
                store: store.clone(),
                jobs: jobs.clone(),
                fetcher: Fetcher::new(files.as_ref().clone()).unwrap(),
                model: model.clone(),
                interval,
                recovery,
                concurrency: watchtower::worker::CONCURRENCY,
            }
            .run(stop.clone()),
        );
        let app = api::router(Api {
            store: store.clone(),
            jobs,
            files,
        });
        let (status, body) = call(
            &app,
            "POST",
            "/portals",
            Some(json!({"url": format!("{base}{path}")})),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let portal = serde_json::from_slice::<Value>(&body).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let harness = Self {
            app,
            store,
            site,
            model,
            portal,
            stop,
            worker,
            server,
            _data: data,
        };
        harness.settle(None).await;
        harness
    }

    /// Waits until `run` (or, without one, the first run) and any repair it started have finished.
    pub async fn settle(&self, run: Option<&str>) {
        let path = format!("/portals/{}/runs", self.portal);
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let runs = get_json(&self.app, &path).await;
                let runs = runs.as_array().unwrap();
                let listed = match run {
                    Some(id) => runs.iter().any(|row| row["id"] == id),
                    None => !runs.is_empty(),
                };
                if listed && runs.iter().all(|row| !row["result"].is_null()) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("runs did not finish");
    }

    pub async fn change(&self, edit: impl FnOnce(&mut Layout)) {
        edit(&mut self.site.lock().unwrap());
        let (status, body) = call(
            &self.app,
            "POST",
            &format!("/portals/{}/refresh", self.portal),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let run: Value = serde_json::from_slice(&body).unwrap();
        self.settle(run["runId"].as_str()).await;
    }

    /// Asks for a refresh and returns its run ID without waiting.
    pub async fn refresh(&self) -> String {
        let (status, body) = call(
            &self.app,
            "POST",
            &format!("/portals/{}/refresh", self.portal),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        serde_json::from_slice::<Value>(&body).unwrap()["runId"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Waits until the portal entry matches, for at most `seconds`.
    pub async fn wait_for(&self, seconds: u64, matches: impl Fn(&Value) -> bool) -> Value {
        tokio::time::timeout(Duration::from_secs(seconds), async {
            loop {
                let portal = self.portal().await;
                if matches(&portal) {
                    return portal;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("portal did not reach the expected state"))
    }

    pub async fn runs(&self) -> Vec<Value> {
        get_json(&self.app, &format!("/portals/{}/runs", self.portal))
            .await
            .as_array()
            .unwrap()
            .clone()
    }

    pub async fn portal(&self) -> Value {
        get_json(&self.app, &format!("/portals/{}", self.portal)).await
    }

    pub async fn tenders(&self) -> Value {
        get_json(&self.app, &format!("/portals/{}/tenders", self.portal)).await
    }

    pub async fn tender(&self, id: &str) -> Value {
        self.tenders()
            .await
            .as_array()
            .unwrap()
            .iter()
            .find(|tender| tender["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("tender {id} is not published"))
    }

    /// Prints what happened, for the case table in the review.
    pub async fn observe(&self, case: &str) {
        let portal = self.portal().await;
        let tenders = self.tenders().await;
        let summary: Vec<String> = tenders
            .as_array()
            .unwrap()
            .iter()
            .map(|tender| {
                let files: Vec<_> = tender["documents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|doc| doc["fileName"].as_str().unwrap().to_owned())
                    .collect();
                format!(
                    "{}:{}:{}:{}",
                    tender["id"].as_str().unwrap_or("null"),
                    tender["status"].as_str().unwrap(),
                    tender["buyerName"].as_str().unwrap_or("null"),
                    files.join("+")
                )
            })
            .collect();
        let runs = get_json(&self.app, &format!("/portals/{}/runs", self.portal)).await;
        let history: Vec<String> = runs
            .as_array()
            .unwrap()
            .iter()
            .map(|run| {
                format!(
                    "{}={}",
                    run["kind"].as_str().unwrap(),
                    run["result"].as_str().unwrap_or("-")
                )
            })
            .collect();
        eprintln!(
            "[{case}] state={} reason={:?} learns={} verifies={} runs=[{}] tenders=[{}]",
            portal["state"].as_str().unwrap(),
            portal["reason"].as_str().unwrap(),
            self.model.learns.load(Ordering::SeqCst),
            self.model.verifies.load(Ordering::SeqCst),
            history.join(", "),
            summary.join(", "),
        );
    }

    pub async fn shutdown(self) {
        self.stop.cancel();
        self.worker.await.unwrap();
        self.server.abort();
    }
}

pub fn ids(tenders: &Value) -> Vec<String> {
    let mut ids: Vec<_> = tenders
        .as_array()
        .unwrap()
        .iter()
        .map(|tender| tender["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

pub fn file_names(tender: &Value) -> Vec<String> {
    tender["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|doc| doc["fileName"].as_str().unwrap().to_owned())
        .collect()
}
