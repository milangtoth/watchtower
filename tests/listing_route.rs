//! Routes to a listing limited to open tenders when the full table is too long, as on cosinex
//! Satellite portals: a search form filter read as a GET link, whose result pages keep the
//! filter in a session cookie and link onward through `onclick` handlers. A first page that
//! already links past the reader's page limit fails at once instead of after 250 requests.
//! Everything is served locally; there are no live requests and no model.

use axum::{
    Router,
    extract::{RawQuery, State},
    http::{HeaderMap, header},
    response::{Html, IntoResponse},
    routing::get,
};
use std::sync::{Arc, Mutex};
use watchtower::{domain::ReadFailure, fetch::Fetcher, recipe::Recipe};

type Requests = Arc<Mutex<Vec<String>>>;

async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await });
    format!("http://{address}")
}

fn table(rows: &[(&str, &str)], next: &str, footer: &str) -> String {
    let rows: String = rows
        .iter()
        .map(|(id, title)| {
            format!(r#"<tr><td class="id">{id}</td><td class="title">{title}</td></tr>"#)
        })
        .collect();
    format!(
        r#"<html><body><main><table><thead><tr><th>Id</th><th>Title</th></tr></thead>
        <tbody>{rows}</tbody><tfoot><tr><td>{next}<span id="total">{footer}</span></td></tr></tfoot>
        </table></main></body></html>"#
    )
}

fn recipe(value: serde_json::Value) -> Recipe {
    serde_json::from_value(value).unwrap()
}

fn record(requests: &Requests, path: &str, query: Option<String>, headers: &HeaderMap) {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("-");
    requests.lock().unwrap().push(format!(
        "{path}?{} cookie={cookie}",
        query.unwrap_or_default()
    ));
}

/// A search that keeps its filter in the session: `/search?status=open` sets the session and
/// shows page 1; `/table?page=2` shows page 2 of that search, or page 2 of the unfiltered table
/// when the session cookie is missing.
fn session_portal(requests: Requests) -> Router {
    Router::new()
        .route(
            "/search",
            get(
                |State(requests): State<Requests>, RawQuery(query): RawQuery, headers: HeaderMap| async move {
                    record(&requests, "/search", query.clone(), &headers);
                    assert_eq!(query.as_deref(), Some("method=search&status=open"));
                    let next = r#"<a class="browseForward" href="javascript:void(0);" onclick="setTargetAndSubmit('/table?page=2')">&#160;</a>"#;
                    (
                        [(header::SET_COOKIE, "SESSION=open-search; Path=/; HttpOnly")],
                        Html(table(&[("A", "Road survey"), ("B", "Park works")], next, "3 tenders")),
                    )
                },
            ),
        )
        .route(
            "/table",
            get(
                |State(requests): State<Requests>, RawQuery(query): RawQuery, headers: HeaderMap| async move {
                    record(&requests, "/table", query, &headers);
                    let session = headers
                        .get(header::COOKIE)
                        .and_then(|value| value.to_str().ok())
                        == Some("SESSION=open-search");
                    let last = r#"<a class="browseForwardGhost" href="javascript:void(0);">&#160;</a>"#;
                    if session {
                        Html(table(&[("C", "School roof")], last, "3 tenders")).into_response()
                    } else {
                        Html(table(&[("X", "Awarded contract")], last, "6751 tenders"))
                            .into_response()
                    }
                },
            ),
        )
        .with_state(requests)
}

#[tokio::test]
async fn search_filter_pages_through_its_session_and_onclick_links() {
    let requests = Requests::default();
    let base = serve(session_portal(requests.clone())).await;
    let portal = reqwest::Url::parse(&format!("{base}/welcome")).unwrap();
    let recipe = recipe(serde_json::json!({
        "start_url": "/search?method=search&status=open",
        "listing": "html", "items": "tbody tr",
        "next": {"path": "a.browseForward", "attribute": "onclick"},
        "total": {"path": "#total"},
        "list_fields": {"id": {"path": "td.id"}, "title": {"path": "td.title"}}
    }));
    let files = tempfile::tempdir().unwrap();
    let http = Fetcher::new(files.path().into()).unwrap().portal(&portal);

    let read = watchtower::recipe::read(&http, &portal, &recipe)
        .await
        .unwrap();

    let ids: Vec<_> = read.tenders.iter().filter_map(|t| t.id.clone()).collect();
    assert_eq!(ids, ["A", "B", "C"]);
    assert_eq!(read.evidence.end_of_list, "total");
    assert_eq!(
        requests.lock().unwrap().clone(),
        [
            "/search?method=search&status=open cookie=-",
            "/table?page=2 cookie=SESSION=open-search",
        ]
    );
}

#[tokio::test]
async fn session_cookies_stay_with_one_portal_run() {
    let requests = Requests::default();
    let base = serve(session_portal(requests.clone())).await;
    let portal = reqwest::Url::parse(&format!("{base}/welcome")).unwrap();
    let files = tempfile::tempdir().unwrap();
    let fetcher = Fetcher::new(files.path().into()).unwrap();
    let first = fetcher.portal(&portal);
    first
        .page(portal.join("/search?method=search&status=open").unwrap())
        .await
        .unwrap();
    // A later run starts without the earlier run's session.
    let later = fetcher.portal(&portal);
    let page = later
        .page(portal.join("/table?page=2").unwrap())
        .await
        .unwrap();
    assert!(page.body.contains("Awarded contract"));
    assert_eq!(
        requests.lock().unwrap().last().unwrap(),
        "/table?page=2 cookie=-"
    );
}

#[tokio::test]
async fn first_page_linking_past_the_page_limit_fails_without_reading_on() {
    let requests = Requests::default();
    let router = Router::new()
        .route(
            "/welcome",
            get(
                |State(requests): State<Requests>, RawQuery(query): RawQuery, headers: HeaderMap| async move {
                    record(&requests, "/welcome", query, &headers);
                    let links = r#"<a class="browseForward" href="/welcome?method=showTable&amp;page=2">&#160;</a>
                        <a class="browseLast" href="/welcome?method=showTable&amp;page=338">&#160;</a>
                        <a href="/welcome?method=showTable&amp;sort=title">Title</a>"#;
                    Html(table(&[("A", "Road survey"), ("B", "Park works")], links, "Page 1 of 338"))
                },
            ),
        )
        .with_state(requests.clone());
    let base = serve(router).await;
    let portal = reqwest::Url::parse(&format!("{base}/welcome")).unwrap();
    let recipe = recipe(serde_json::json!({
        "listing": "html", "items": "tbody tr",
        "next": {"path": "a.browseForward", "attribute": "href"},
        "list_fields": {"id": {"path": "td.id"}, "title": {"path": "td.title"}}
    }));
    let files = tempfile::tempdir().unwrap();
    let http = Fetcher::new(files.path().into()).unwrap().portal(&portal);

    let failure = watchtower::recipe::read(&http, &portal, &recipe)
        .await
        .unwrap_err();

    assert!(matches!(failure, ReadFailure::Incomplete(_)), "{failure}");
    let reason = failure.to_string();
    assert!(reason.contains("338 pages, more than 250"), "{reason}");
    assert!(reason.contains("search form filter"), "{reason}");
    assert_eq!(requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn total_that_needs_more_pages_than_the_limit_fails_on_the_first_page() {
    let requests = Requests::default();
    let router = Router::new()
        .route(
            "/list",
            get(
                |State(requests): State<Requests>,
                 RawQuery(query): RawQuery,
                 headers: HeaderMap| async move {
                    record(&requests, "/list", query, &headers);
                    let next = r#"<a class="next" href="/list?cursor=abc">Next</a>"#;
                    Html(table(
                        &[("A", "Road survey"), ("B", "Park works")],
                        next,
                        "1000 results",
                    ))
                },
            ),
        )
        .with_state(requests.clone());
    let base = serve(router).await;
    let portal = reqwest::Url::parse(&format!("{base}/list")).unwrap();
    let recipe = recipe(serde_json::json!({
        "listing": "html", "items": "tbody tr",
        "next": {"path": "a.next", "attribute": "href"},
        "total": {"path": "#total"},
        "list_fields": {"id": {"path": "td.id"}, "title": {"path": "td.title"}}
    }));
    let files = tempfile::tempdir().unwrap();
    let http = Fetcher::new(files.path().into()).unwrap().portal(&portal);

    let reason = watchtower::recipe::read(&http, &portal, &recipe)
        .await
        .unwrap_err()
        .to_string();

    assert!(reason.contains("500 pages, more than 250"), "{reason}");
    assert_eq!(requests.lock().unwrap().len(), 1);
}
