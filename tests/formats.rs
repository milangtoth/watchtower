//! Connection methods beyond HTML listings: RSS feeds, sitemaps and OCDS packages, and repair
//! falling back from one method to the next.

mod common;

use common::*;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use watchtower::domain::ConnectionMethod::{self, Feed, Html, Ocds, Sitemap};

/// Rules a model would learn for the fixture's tender detail pages.
fn detail_fields() -> Value {
    json!({
        "id": {"path": "main", "attribute": "data-id"},
        "buyerName": {"path": ".buyer"},
        "submissionDeadline": {"path": "time", "attribute": "datetime"}
    })
}

fn feed_recipe() -> Value {
    json!({
        "listing": "feed", "detail_fields": detail_fields(),
        "direct_documents": {"path": "a.file", "attribute": "href"}
    })
}

fn sitemap_recipe() -> Value {
    let mut fields = detail_fields();
    fields["title"] = json!({"path": "h1"});
    json!({
        "listing": "sitemap", "url_pattern": "/tenders/", "detail_fields": fields,
        "direct_documents": {"path": "a.file", "attribute": "href"}
    })
}

/// The fixture's two tenders, compared with values written out here.
async fn assert_fixture_tenders(h: &Harness) {
    let tenders = h.tenders().await;
    assert_eq!(ids(&tenders), ["A", "B"]);
    for (id, title, buyer, file) in [
        ("A", "Road survey", "City of Alpha", "a.pdf"),
        ("B", "Park works", "City of Beta", "b.pdf"),
    ] {
        let tender = h.tender(id).await;
        assert_eq!(tender["title"], title);
        assert_eq!(tender["buyerName"], buyer);
        assert_eq!(tender["submissionDeadline"], DEADLINE);
        assert_eq!(tender["status"], "open");
        assert_eq!(file_names(&tender), [file]);
        assert_eq!(tender["documents"][0]["sha256"], sha(file));
        let (_, bytes) = call(
            &h.app,
            "GET",
            tender["documents"][0]["path"].as_str().unwrap(),
            None,
        )
        .await;
        assert_eq!(bytes, file_bytes(file));
    }
}

async fn chosen(h: &Harness) -> (ConnectionMethod, u32, String) {
    let decision = h.store.latest_decision(&h.portal).await.unwrap().unwrap();
    (decision.chosen.method, decision.attempt, decision.rationale)
}

#[tokio::test]
async fn feed_entries_and_detail_pages_are_published_and_rereads_need_no_model() {
    let h = Harness::attach_connected(
        Layout::two_pages(),
        feed_recipe(),
        vec![(Feed, "/feed.xml")],
    )
    .await;
    h.observe("feed").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_fixture_tenders(&h).await;
    assert_eq!(chosen(&h).await.0, Feed);
    let before = h.tenders().await;
    let calls = (
        h.model.learns.load(Ordering::SeqCst),
        h.model.verifies.load(Ordering::SeqCst),
    );
    assert_eq!(calls, (1, 1));

    h.change(|_| {}).await;
    assert_eq!(
        h.tenders().await,
        before,
        "an unchanged feed changes nothing"
    );
    assert_eq!(
        (
            h.model.learns.load(Ordering::SeqCst),
            h.model.verifies.load(Ordering::SeqCst)
        ),
        calls,
        "a healthy repeat read makes no model call"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn sitemap_index_with_gzip_child_keeps_only_matching_tender_pages() {
    let h = Harness::attach_connected(
        Layout::two_pages(),
        sitemap_recipe(),
        vec![(Sitemap, "/sitemap.xml")],
    )
    .await;
    h.observe("sitemap").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_fixture_tenders(&h).await;
    assert_eq!(chosen(&h).await.0, Sitemap);
    h.shutdown().await;
}

#[tokio::test]
async fn ocds_preset_maps_fields_follows_next_links_and_needs_no_learning() {
    let h =
        Harness::attach_connected(Layout::two_pages(), html_recipe(), vec![(Ocds, "/ocds")]).await;
    h.observe("ocds").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_fixture_tenders(&h).await;
    let alpha = h.tender("A").await;
    assert_eq!(alpha["procedureType"], "open");
    assert_eq!(alpha["contractNature"], "services");
    assert_eq!(alpha["cpvCodes"], json!(["72000000"]));
    assert_eq!(alpha["publicationDate"], "2026-11-01");
    assert_eq!(
        h.model.learns.load(Ordering::SeqCst),
        0,
        "OCDS needs no learned recipe"
    );
    assert_eq!(h.model.verifies.load(Ordering::SeqCst), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn repair_falls_back_to_the_next_method_after_both_attempts_fail() {
    let h = Harness::attach_connected(
        Layout::two_pages(),
        html_recipe(),
        vec![(Html, "/"), (Ocds, "/ocds")],
    )
    .await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(chosen(&h).await.0, Html);
    // The listing total stops matching, so every HTML recipe reads an incomplete listing.
    h.change(|layout| layout.total = Some("5".into())).await;
    h.observe("fallback").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "serving");
    assert_eq!(portal["reason"], "Last read verified");
    let (method, attempt, rationale) = chosen(&h).await;
    assert_eq!((method, attempt), (Ocds, 2));
    assert!(
        rationale.starts_with("HTML failed during repair"),
        "{rationale}"
    );
    assert_eq!(
        h.model.learns.load(Ordering::SeqCst),
        3,
        "one attach and two HTML repair attempts; OCDS needs no learning"
    );
    let kinds: Vec<_> = h
        .runs()
        .await
        .iter()
        .map(|run| {
            format!(
                "{}={}",
                run["kind"].as_str().unwrap(),
                run["result"].as_str().unwrap()
            )
        })
        .collect();
    assert_eq!(kinds, ["read=ok", "read=failed", "repair=ok"]);
    assert_eq!(ids(&h.tenders().await), ["A", "B"]);
    h.shutdown().await;
}

#[tokio::test]
async fn repair_fails_naming_both_methods_and_keeps_serving_the_last_tenders() {
    let h = Harness::attach_connected(
        Layout::two_pages(),
        html_recipe(),
        vec![(Html, "/"), (Ocds, "/ocds")],
    )
    .await;
    let before = h.tenders().await;
    h.change(|layout| {
        layout.total = Some("5".into());
        layout.ocds_broken = true;
    })
    .await;
    h.observe("both-methods-failed").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "failed");
    let reason = portal["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("HTML and OCDS both failed: "),
        "{reason}"
    );
    assert_eq!(h.tenders().await, before);
    assert_eq!(chosen(&h).await.1, 2);
    h.shutdown().await;
}
