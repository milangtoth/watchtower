//! Reader fixes from issue #18: pagination selectors that also match "first" and "previous"
//! links, JSON listings paged by number, HTML detail pages behind a JSON listing, and OCDS APIs
//! that stop answering with JSON part-way through.

mod common;

use common::*;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use watchtower::domain::ConnectionMethod::Ocds;

fn gamma() -> Row {
    Row {
        id: "C",
        title: "School roof",
        buyer: "City of Gamma",
        files: vec!["c.pdf"],
    }
}

/// A JSON listing like Catalonia's: rows under `/content`, a total, no next link.
fn paged_json_recipe(total: bool) -> Value {
    let mut recipe = json!({
        "listing": "json", "items": "/content", "page_parameter": "page",
        "list_fields": {
            "id": {"path": "/id"}, "title": {"path": "/title"}, "buyerName": {"path": "/buyer"},
            "submissionDeadline": {"path": "/deadline"}, "url": {"path": "/url"}
        },
        "direct_documents": {"path": "/files"}
    });
    if total {
        recipe["total"] = json!({"path": "/totalElements"});
    }
    recipe
}

#[tokio::test]
async fn next_selector_that_also_matches_first_and_previous_links_reads_every_page() {
    let mut layout = Layout::two_pages();
    layout.pages = vec![vec![alpha()], vec![beta()], vec![gamma()]];
    layout.total = Some("3".into());
    layout.nav_links = true;
    let h = Harness::attach(layout, html_recipe(), "/").await;
    h.observe("nav-links").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "serving", "{portal}");
    assert_eq!(ids(&h.tenders().await), ["A", "B", "C"]);
    h.shutdown().await;
}

#[tokio::test]
async fn json_listing_paged_by_number_stops_at_the_total() {
    let h = Harness::attach(
        Layout::two_pages(),
        paged_json_recipe(true),
        "/api/paged?page=0",
    )
    .await;
    h.observe("paged-json-total").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(ids(&h.tenders().await), ["A", "B"]);
    h.shutdown().await;
}

#[tokio::test]
async fn json_listing_paged_by_number_without_total_stops_at_an_empty_page() {
    let h = Harness::attach(
        Layout::two_pages(),
        paged_json_recipe(false),
        "/api/paged?page=0",
    )
    .await;
    h.observe("paged-json-empty").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(ids(&h.tenders().await), ["A", "B"]);
    h.shutdown().await;
}

#[tokio::test]
async fn html_detail_page_behind_a_json_listing_gets_an_actionable_reason() {
    let recipe = json!({
        "listing": "json", "items": "/items",
        "detail_link": {"path": "/url"},
        "list_fields": {"id": {"path": "/id"}, "title": {"path": "/title"}},
        "detail_fields": {"buyerName": {"path": "/buyer"}}
    });
    let h = Harness::attach(Layout::one_page(), recipe, "/api").await;
    h.observe("html-detail").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "failed");
    let reason = portal["reason"].as_str().unwrap();
    assert!(
        reason.contains("/tenders/A is HTML, not JSON; set detail_format to \"html\""),
        "{reason}"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn ocds_api_that_stops_answering_json_mid_listing_counts_as_throttling() {
    let mut layout = Layout::two_pages();
    layout.ocds_throttled = true;
    let h = Harness::attach_connected(layout, html_recipe(), vec![(Ocds, "/ocds")]).await;
    h.observe("ocds-throttled").await;
    let portal = h.portal().await;
    assert_eq!(
        portal["state"], "integrating",
        "a throttled attach is retried later"
    );
    let reason = portal["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("Attach paused by a temporary failure: OCDS page 2 at")
            && reason.contains("probably limiting requests"),
        "{reason}"
    );
    assert_eq!(h.model.learns.load(Ordering::SeqCst), 0);
    h.shutdown().await;
}
