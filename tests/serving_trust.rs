//! Reproduces ways a read can look healthy while delivering the wrong tenders.
//!
//! Each case attaches a small fixture portal, changes it, refreshes, and compares the published
//! snapshot with values written out in the test. The expected tenders are enumerated here, never
//! derived from Watchtower's output.
//!
//! `Auditor` stands in for Luna. Like the production `verify` call, it sees at most four parsed
//! tenders and the review the worker built. It rejects a sampled tender whose title, buyer or
//! files differ from the source, and a review that lists unfollowed pagination links. It approves
//! everything else. A case passes only if Watchtower either catches the fault itself or asks the
//! auditor at a point where the fault is visible to it.
//!
//! The reproductions failed before the evidence-based publication rule. Run the file with
//! `cargo test --test serving_trust -- --nocapture` to see the state, reason and model calls
//! each case observed.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use std::sync::atomic::Ordering;

// ---------------------------------------------------------------------------------------------
// Controls: behavior that is correct today and must stay correct.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn healthy_unchanged_refresh_needs_no_model_call() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    let before = h.tenders().await;
    assert_eq!(ids(&before), ["A", "B"]);
    h.change(|_| {}).await;
    h.observe("healthy-refresh").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(
        h.tenders().await,
        before,
        "an unchanged read must not touch lastChangedAt"
    );
    assert_eq!(h.model.learns.load(Ordering::SeqCst), 1);
    assert_eq!(
        h.model.verifies.load(Ordering::SeqCst),
        1,
        "only the first attach is audited"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn tender_the_portal_stops_listing_is_closed() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    let alpha_before = h.tender("A").await;
    h.change(|layout| {
        layout.pages = vec![vec![alpha()]];
        layout.total = Some("1".into());
    })
    .await;
    h.observe("real-removal").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(h.tender("B").await["status"], "closed");
    assert_eq!(h.tender("A").await, alpha_before);
    h.shutdown().await;
}

#[tokio::test]
async fn tender_count_holds_every_tender_and_the_dashboard_shows_the_open_ones() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    h.change(|layout| {
        layout.pages = vec![vec![alpha()]];
        layout.total = Some("1".into());
    })
    .await;
    let tenders = h.tenders().await;
    let tenders = tenders.as_array().unwrap();
    let open = tenders
        .iter()
        .filter(|tender| tender["status"] == "open")
        .count();
    assert_eq!((tenders.len(), open), (2, 1));
    assert_eq!(
        h.portal().await["tenderCount"],
        2,
        "tenderCount equals GET /tenders"
    );
    let (status, page) = call(&h.app, "GET", "/", None).await;
    assert_eq!(status, StatusCode::OK);
    let page = String::from_utf8(page).unwrap();
    assert!(page.contains("' tenders held'") && page.contains("tender.status === 'open'"));
    assert!(
        !page.contains("open tenders"),
        "the dashboard no longer calls every tender open"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn replaced_document_is_published_for_its_own_tender() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    let beta_before = h.tender("B").await;
    let alpha_before = h.tender("A").await;
    h.change(|layout| layout.pages[0][0].files = vec!["a-v2.pdf"])
        .await;
    h.observe("document-replaced").await;
    assert_eq!(h.portal().await["state"], "serving");
    let alpha = h.tender("A").await;
    assert_eq!(file_names(&alpha), ["a-v2.pdf"]);
    assert_eq!(alpha["documents"][0]["sha256"], sha("a-v2.pdf"));
    assert_ne!(alpha["lastChangedAt"], alpha_before["lastChangedAt"]);
    let (status, bytes) = call(
        &h.app,
        "GET",
        alpha["documents"][0]["path"].as_str().unwrap(),
        None,
    )
    .await;
    assert_eq!((status, bytes), (StatusCode::OK, file_bytes("a-v2.pdf")));
    assert_eq!(h.tender("B").await, beta_before);
    h.shutdown().await;
}

#[tokio::test]
async fn pagination_change_caught_by_matching_total_keeps_snapshot() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    let before = h.tenders().await;
    h.change(|layout| layout.next_class = "pager-next").await;
    h.observe("pagination-change-with-total").await;
    assert_eq!(h.tenders().await, before);
    h.shutdown().await;
}

#[tokio::test]
async fn genuinely_empty_listing_closes_open_tenders() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    h.change(|layout| {
        layout.pages = vec![Vec::new()];
        layout.total = Some("0".into());
        layout.empty_marker = true;
    })
    .await;
    h.observe("genuine-empty").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(h.tender("A").await["status"], "closed");
    assert_eq!(h.tender("B").await["status"], "closed");
    h.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Reproductions: each failed before the publication rule. The assertion states the outcome
// the rule must deliver.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
// Before the rule: risk 1: pagination stops at a missing next link and the page-2 tender is closed.
async fn page_two_tender_survives_a_pagination_link_change() {
    let mut layout = Layout::two_pages();
    layout.total = None;
    let h = Harness::attach(layout, without(html_recipe(), &["total"]), "/").await;
    let before = h.tenders().await;
    h.change(|layout| layout.next_class = "pager-next").await;
    h.observe("pagination-change-no-total").await;
    assert_eq!(
        h.tender("B").await["status"],
        "open",
        "B is still on page 2"
    );
    assert_eq!(h.tenders().await, before);
    h.shutdown().await;
}

#[tokio::test]
// Before the rule: risk 1: a total that disappears is ignored, so a broken pagination looks complete.
async fn missing_total_does_not_hide_a_pagination_break() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    let before = h.tenders().await;
    h.change(|layout| {
        layout.total = None;
        layout.next_class = "pager-next";
    })
    .await;
    h.observe("total-missing").await;
    assert_eq!(h.tender("B").await["status"], "open");
    assert_eq!(h.tenders().await, before);
    h.shutdown().await;
}

#[tokio::test]
// Before the rule: risk 1: a total that no longer parses is ignored, so a broken pagination looks complete.
async fn malformed_total_does_not_hide_a_pagination_break() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    let before = h.tenders().await;
    h.change(|layout| {
        layout.total = Some("2 results".into());
        layout.next_class = "pager-next";
    })
    .await;
    h.observe("total-malformed").await;
    assert_eq!(h.tender("B").await["status"], "open");
    assert_eq!(h.tenders().await, before);
    h.shutdown().await;
}

#[tokio::test]
// Before the rule: risk 1: a first attach that never follows pagination is audited on page 1 only and serves.
async fn first_attach_does_not_serve_page_one_as_the_whole_portal() {
    let mut layout = Layout::two_pages();
    layout.total = None;
    let h = Harness::attach(layout, without(html_recipe(), &["total", "next"]), "/").await;
    h.observe("first-attach-missed-pagination").await;
    let state = h.portal().await["state"].clone();
    assert!(
        state != "serving" || ids(&h.tenders().await) == ["A", "B"],
        "serving with an incomplete listing"
    );
    h.shutdown().await;
}

#[tokio::test]
// Before the rule: risk 1, rule-dependent: an empty JSON array with no total or empty marker closes every tender.
async fn bare_empty_json_page_does_not_close_open_tenders() {
    let recipe = json!({
        "listing": "json", "items": "/items",
        "list_fields": {
            "id": {"path": "/id"}, "title": {"path": "/title"}, "buyerName": {"path": "/buyer"},
            "submissionDeadline": {"path": "/deadline"}, "url": {"path": "/url"}
        },
        "direct_documents": {"path": "/files"}
    });
    let h = Harness::attach(Layout::one_page(), recipe, "/api").await;
    let before = h.tenders().await;
    assert_eq!(ids(&before), ["A", "B"]);
    h.change(|layout| layout.json_empty = true).await;
    h.observe("bare-empty-json").await;
    assert_eq!(h.tender("A").await["status"], "open");
    assert_eq!(h.tender("B").await["status"], "open");
    h.shutdown().await;
}

#[tokio::test]
// Before the rule: risk 5: a value from the wrong column is published with no check and no model call.
async fn value_from_a_shifted_column_is_not_published() {
    let h = Harness::attach(Layout::one_page(), html_recipe(), "/").await;
    h.change(|layout| layout.lot_column = true).await;
    h.observe("shifted-column").await;
    assert_eq!(h.tender("A").await["buyerName"], "City of Alpha");
    assert_eq!(h.tender("B").await["buyerName"], "City of Beta");
    h.shutdown().await;
}

#[tokio::test]
// Before the rule: risk 5: files moved to the wrong tender keep the file count and are published unchecked.
async fn document_attached_to_the_wrong_tender_is_not_published() {
    let h = Harness::attach(Layout::one_page(), html_recipe(), "/").await;
    h.change(|layout| layout.shift_documents = true).await;
    h.observe("documents-shifted").await;
    assert_eq!(file_names(&h.tender("A").await), ["a.pdf"]);
    assert_eq!(file_names(&h.tender("B").await), ["b.pdf"]);
    h.shutdown().await;
}
