//! Connection methods beyond HTML listings: RSS feeds, sitemaps and OCDS packages, and repair
//! and attach falling back from one method to the next.

mod common;

use common::*;
use serde_json::{Value, json};
use std::sync::atomic::Ordering;
use watchtower::domain::ConnectionMethod::{self, Feed, Html, Ocds, Sitemap, Ted};

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

fn days_ago(days: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339()
}

#[tokio::test]
async fn windowed_ocds_read_keeps_open_tenders_older_than_its_window() {
    let mut layout = Layout::two_pages();
    layout.ocds_dates = [("A", days_ago(10)), ("B", days_ago(10))].into();
    let h = Harness::attach_connected(
        layout,
        html_recipe(),
        vec![(
            Ocds,
            "/ocds/since?updatedFrom=2026-01-01T00:00:00&stages=tender",
        )],
    )
    .await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(ids(&h.tenders().await), ["A", "B"]);
    let beta_before = h.tender("B").await;

    // B was published 120 days ago and not updated since, so the window no longer returns it.
    // Its deadline is still ahead. A reports a cancellation within the window.
    h.change(|layout| {
        layout.ocds_dates.insert("B", days_ago(120));
        layout.ocds_statuses.insert("A", "cancelled");
    })
    .await;
    h.observe("ocds-window-aged-out").await;
    assert_eq!(h.portal().await["state"], "serving");
    let beta = h.tender("B").await;
    assert_eq!(
        beta["status"], "open",
        "absence from a window is no closure"
    );
    assert_eq!(beta, beta_before);
    assert_eq!(h.tender("A").await["status"], "withdrawn");
    h.shutdown().await;
}

#[tokio::test]
async fn ocds_read_without_a_window_closes_tenders_it_no_longer_lists() {
    let h =
        Harness::attach_connected(Layout::two_pages(), html_recipe(), vec![(Ocds, "/ocds")]).await;
    h.change(|layout| layout.pages = vec![vec![alpha()], Vec::new()])
        .await;
    h.observe("ocds-removal").await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(h.tender("B").await["status"], "closed");
    assert_eq!(h.tender("A").await["status"], "open");
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
    assert_eq!(h.model.decisions.load(Ordering::SeqCst), 2);
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

fn run_history(runs: &[Value]) -> Vec<String> {
    runs.iter()
        .map(|run| {
            format!(
                "{}={}",
                run["kind"].as_str().unwrap(),
                run["result"].as_str().unwrap()
            )
        })
        .collect()
}

async fn run_reasons(h: &Harness) -> Vec<String> {
    h.store
        .run_history(&h.portal)
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|run| run.reason.unwrap_or_default())
        .collect()
}

/// A sitemap whose pattern matches no tender page, as GOV.UK's did: it reads zero rows.
fn empty_sitemap_recipe() -> Value {
    let mut recipe = sitemap_recipe();
    recipe["url_pattern"] = json!("/no-such-tenders/");
    recipe
}

#[tokio::test]
async fn first_attach_verifies_a_listing_with_an_explicit_zero_total() {
    let layout = Layout {
        pages: vec![Vec::new()],
        total: Some("0".into()),
        ..Layout::one_page()
    };
    let h = Harness::attach_connected(layout, html_recipe(), vec![(Html, "/")]).await;
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!(h.tenders().await, json!([]));
    assert_eq!(h.model.verifies.load(Ordering::SeqCst), 1);
    assert_eq!(run_history(&h.runs().await), ["read=ok"]);
    h.shutdown().await;
}

#[tokio::test]
async fn first_attach_tries_another_url_for_the_same_method() {
    let h = Harness::attach_connected(
        Layout::two_pages(),
        feed_recipe(),
        vec![
            (Feed, "/sitemap.xml"),
            (Feed, "/sitemap.xml"),
            (Feed, "/feed.xml"),
        ],
    )
    .await;
    assert_eq!(h.portal().await["state"], "serving");
    let decision = h.store.latest_decision(&h.portal).await.unwrap().unwrap();
    assert_eq!((decision.chosen.method, decision.attempt), (Feed, 2));
    assert!(decision.chosen.entry_url.ends_with("/feed.xml"));
    assert_eq!(run_history(&h.runs().await), ["read=failed", "read=ok"]);
    assert_fixture_tenders(&h).await;
    h.shutdown().await;
}

#[tokio::test]
async fn repair_replaces_a_saved_ted_choice_before_learning() {
    let h = Harness::attach_connected(
        Layout::two_pages(),
        html_recipe(),
        vec![(Html, "/"), (Ocds, "/ocds")],
    )
    .await;
    let mut saved = h.store.latest_decision(&h.portal).await.unwrap().unwrap();
    let job = watchtower::db::Job {
        id: h.runs().await[0]["id"].as_str().unwrap().into(),
        portal_id: h.portal.clone(),
        generation: 1,
        url: saved.chosen.entry_url.clone(),
        recipe: None,
        last_success_at: None,
    };
    saved.chosen.method = Ted;
    saved.chosen.http_status = 0;
    h.store.record_decision(&job, &saved).await.unwrap();
    let learns = h.model.learns.load(Ordering::SeqCst);
    h.change(|layout| layout.total = Some("5".into())).await;
    let decision = h.store.latest_decision(&h.portal).await.unwrap().unwrap();
    assert_eq!(h.portal().await["state"], "serving");
    assert_eq!((decision.chosen.method, decision.attempt), (Ocds, 2));
    assert_eq!(h.model.learns.load(Ordering::SeqCst), learns);
    assert!(
        decision
            .alternatives
            .iter()
            .any(|c| c.method == Ted && c.score == 0.0)
    );
    h.shutdown().await;
}

#[tokio::test]
async fn first_attach_falls_back_when_the_chosen_method_reads_no_tenders() {
    let h = Harness::attach_methods(
        Layout::two_pages(),
        vec![(Sitemap, "/sitemap.xml"), (Feed, "/feed.xml")],
        vec![(Sitemap, empty_sitemap_recipe()), (Feed, feed_recipe())],
    )
    .await;
    h.observe("attach-fallback").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "serving", "{portal}");
    assert_fixture_tenders(&h).await;
    let (method, attempt, rationale) = chosen(&h).await;
    assert_eq!((method, attempt), (Feed, 2));
    assert!(
        rationale.starts_with("Sitemap failed during attach (Sitemap read found no tenders)"),
        "{rationale}"
    );
    assert_eq!(
        h.model.learns.load(Ordering::SeqCst),
        3,
        "two sitemap attempts and one feed attempt"
    );
    assert_eq!(
        run_history(&h.runs().await),
        ["read=failed", "read=ok"],
        "each method is a run of its own"
    );
    let reasons = run_reasons(&h).await;
    assert!(
        reasons[0].starts_with("Sitemap failed: Sitemap read found no tenders. Trying Feed"),
        "{reasons:?}"
    );
    assert!(
        reasons[1].starts_with("Attached and verified via Feed after Sitemap failed. 2 tenders"),
        "{reasons:?}"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn failed_attach_names_every_method_and_never_tries_ted() {
    let mut layout = Layout::two_pages();
    layout.ocds_broken = true;
    let h = Harness::attach_methods(
        layout,
        vec![(Ted, "/ted"), (Sitemap, "/sitemap.xml"), (Ocds, "/ocds")],
        vec![(Sitemap, empty_sitemap_recipe())],
    )
    .await;
    h.observe("attach-all-methods-failed").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "failed");
    let reason = portal["reason"].as_str().unwrap();
    assert!(
        reason.starts_with(
            "Attach tried Sitemap and OCDS; none delivered verified tenders. \
             Sitemap: Sitemap read found no tenders; OCDS: "
        ),
        "{reason}"
    );
    assert!(!reason.contains("TED"), "{reason}");
    let decision = h.store.latest_decision(&h.portal).await.unwrap().unwrap();
    assert_eq!((decision.chosen.method, decision.attempt), (Ocds, 2));
    assert!(
        decision.alternatives.iter().any(|c| c.method == Ted),
        "TED stays in the recorded evidence"
    );
    assert_eq!(run_history(&h.runs().await), ["read=failed", "read=failed"]);
    let reasons = run_reasons(&h).await;
    assert!(reasons[0].starts_with("Sitemap failed: "), "{reasons:?}");
    assert_eq!(reasons[1], reason, "the last run records the final reason");
    assert_eq!(h.tenders().await, json!([]));
    h.shutdown().await;
}

#[tokio::test]
async fn login_wall_on_attach_fails_at_once_without_falling_back() {
    let mut layout = Layout::two_pages();
    layout.login_wall = true;
    let h = Harness::attach_methods(
        layout,
        vec![(Html, "/"), (Feed, "/feed.xml")],
        vec![(Feed, feed_recipe())],
    )
    .await;
    h.observe("attach-login-wall").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "failed");
    let reason = portal["reason"].as_str().unwrap();
    assert!(reason.contains("login"), "{reason}");
    assert_eq!(h.model.decisions.load(Ordering::SeqCst), 0);
    assert_eq!(
        h.model.learns.load(Ordering::SeqCst),
        0,
        "no method was tried"
    );
    assert_eq!(run_history(&h.runs().await), ["read=failed"]);
    h.shutdown().await;
}

#[tokio::test]
async fn a_method_declared_unsupported_costs_one_learn_call_before_falling_back() {
    let h = Harness::attach_faked(
        Layout::two_pages(),
        vec![(Sitemap, "/sitemap.xml"), (Feed, "/feed.xml")],
        vec![(Feed, feed_recipe())],
        Fakes {
            unsupported: vec![Sitemap],
            ..Fakes::default()
        },
    )
    .await;
    h.observe("attach-unsupported").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "serving", "{portal}");
    assert_fixture_tenders(&h).await;
    assert_eq!(chosen(&h).await.0, Feed);
    assert_eq!(
        h.model.learns.load(Ordering::SeqCst),
        2,
        "one learn call for the unsupported sitemap, one for the feed"
    );
    let reasons = run_reasons(&h).await;
    assert!(
        reasons[0].starts_with(
            "Sitemap failed: Learning stopped after 1 model call(s), method unsupported"
        ),
        "{reasons:?}"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a_fallback_without_probe_evidence_is_skipped_unless_it_is_the_last() {
    // HTML at "/" would read the fixture, but its probe sample showed no items or pagination,
    // so the feed is tried first.
    let h = Harness::attach_faked(
        Layout::two_pages(),
        vec![(Sitemap, "/sitemap.xml"), (Html, "/"), (Feed, "/feed.xml")],
        vec![(Sitemap, empty_sitemap_recipe()), (Feed, feed_recipe())],
        Fakes {
            bare: vec!["/"],
            ..Fakes::default()
        },
    )
    .await;
    h.observe("attach-skip-bare").await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "serving", "{portal}");
    let (method, attempt, rationale) = chosen(&h).await;
    assert_eq!((method, attempt), (Feed, 2));
    assert!(rationale.contains("Skipped HTML at "), "{rationale}");
    assert_eq!(
        h.model.learns.load(Ordering::SeqCst),
        3,
        "two sitemap attempts and one feed attempt, none for the bare HTML candidate"
    );
    let reasons = run_reasons(&h).await;
    assert!(
        reasons[0].starts_with(
            "Sitemap failed: Sitemap read found no tenders. Trying Feed. Skipped HTML at "
        ),
        "{reasons:?}"
    );
    assert!(
        reasons[0].contains("the probe sampled no items or pagination there"),
        "{reasons:?}"
    );
    h.shutdown().await;

    // When the bare candidate is the only one left, it is still tried.
    let h = Harness::attach_faked(
        Layout::two_pages(),
        vec![(Sitemap, "/sitemap.xml"), (Html, "/")],
        vec![(Sitemap, empty_sitemap_recipe())],
        Fakes {
            bare: vec!["/"],
            ..Fakes::default()
        },
    )
    .await;
    let portal = h.portal().await;
    assert_eq!(portal["state"], "serving", "{portal}");
    assert_eq!(chosen(&h).await.0, Html);
    assert_fixture_tenders(&h).await;
    h.shutdown().await;
}
