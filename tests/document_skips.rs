//! One bad document must not stop delivery, and a broken document route must still count as a
//! break.
//!
//! The fixture answers `/files/gone-*` with HTTP 404, `/files/login-*` with a sign-in page
//! (HTTP 401) and `/files/corrupt-*.zip` with a ZIP cut short before its central directory. Expected files and hashes are written out here.

mod common;

use axum::http::StatusCode;
use common::*;

fn row(id: &'static str, title: &'static str, buyer: &'static str, files: &[&'static str]) -> Row {
    Row {
        id,
        title,
        buyer,
        files: files.to_vec(),
    }
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

#[tokio::test]
async fn a_dead_link_and_a_corrupt_zip_are_skipped_and_the_rest_is_served() {
    let layout = Layout {
        pages: vec![
            vec![row(
                "A",
                "Road survey",
                "City of Alpha",
                &["a.pdf", "gone-a.pdf"],
            )],
            vec![row(
                "B",
                "Park works",
                "City of Beta",
                &["b.pdf", "corrupt-b.zip"],
            )],
        ],
        ..Layout::two_pages()
    };
    let h = Harness::attach(layout, html_recipe(), "/").await;
    h.observe("one-bad-file-each").await;

    let portal = h.portal().await;
    assert_eq!(portal["state"], "serving", "{portal}");
    assert_eq!(portal["reason"], "Last read verified, 2 documents skipped");
    for (id, name) in [("A", "a.pdf"), ("B", "b.pdf")] {
        let tender = h.tender(id).await;
        assert_eq!(file_names(&tender), [name], "{tender}");
        let document = &tender["documents"][0];
        assert_eq!(document["sha256"], sha(name));
        assert_eq!(document["sizeBytes"], file_bytes(name).len());
        let (status, bytes) = call(&h.app, "GET", document["path"].as_str().unwrap(), None).await;
        assert_eq!((status, bytes), (StatusCode::OK, file_bytes(name)));
    }

    let reasons = run_reasons(&h).await;
    let read = reasons
        .iter()
        .find(|reason| reason.contains("held"))
        .unwrap_or_else(|| panic!("no published read in {reasons:?}"));
    assert!(read.contains("2 documents skipped"), "{read}");
    assert!(
        read.contains("gone-a.pdf: skipped, portal returned HTTP 404"),
        "{read}"
    );
    assert!(
        read.contains("corrupt-b.zip: skipped, unreadable archive"),
        "{read}"
    );

    // The operator sees the same account in the dashboard's run history.
    let (status, history) = call(
        &h.app,
        "GET",
        &format!("/dashboard/portals/{}/history", h.portal),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let history = String::from_utf8(history).unwrap();
    assert!(history.contains("2 documents skipped"), "{history}");
    assert!(history.contains("gone-a.pdf"), "{history}");
    h.shutdown().await;
}

#[tokio::test]
async fn every_document_link_breaking_is_a_break_and_keeps_the_snapshot() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    assert_eq!(h.portal().await["state"], "serving");
    let before = h.tenders().await;

    h.change(|layout| {
        layout.pages[0][0].files = vec!["gone-a.pdf"];
        layout.pages[1][0].files = vec!["corrupt-b.zip"];
    })
    .await;
    h.observe("every-document-broken").await;

    let portal = h.portal().await;
    assert_ne!(portal["state"], "serving", "{portal}");
    assert_eq!(
        h.tenders().await,
        before,
        "the last verified tenders and files stay served"
    );
    let reasons = run_reasons(&h).await;
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("2 of 2 document links failed")),
        "{reasons:?}"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn one_document_behind_a_login_is_skipped_like_a_dead_link() {
    let layout = Layout {
        pages: vec![
            vec![row(
                "A",
                "Road survey",
                "City of Alpha",
                &["a.pdf", "login-a.pdf"],
            )],
            vec![row("B", "Park works", "City of Beta", &["b.pdf"])],
        ],
        ..Layout::two_pages()
    };
    let h = Harness::attach(layout, html_recipe(), "/").await;
    h.observe("one-gated-file").await;

    let portal = h.portal().await;
    assert_eq!(portal["state"], "serving", "{portal}");
    assert_eq!(portal["reason"], "Last read verified, 1 document skipped");
    assert_eq!(file_names(&h.tender("A").await), ["a.pdf"]);
    assert_eq!(file_names(&h.tender("B").await), ["b.pdf"]);
    let reasons = run_reasons(&h).await;
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("login-a.pdf: skipped, portal requires a login")),
        "{reasons:?}"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn documents_mostly_behind_a_login_fail_with_the_blocker() {
    let h = Harness::attach(Layout::two_pages(), html_recipe(), "/").await;
    assert_eq!(h.portal().await["state"], "serving");
    let before = h.tenders().await;

    h.change(|layout| {
        layout.pages[0][0].files = vec!["login-a.pdf"];
        layout.pages[1][0].files = vec!["login-b.pdf"];
    })
    .await;
    h.observe("every-document-gated").await;

    let portal = h.portal().await;
    assert_eq!(portal["state"], "failed", "{portal}");
    let reason = portal["reason"].as_str().unwrap();
    assert!(
        reason.contains("2 of 2 document links are behind a login or CAPTCHA"),
        "{reason}"
    );
    assert_eq!(
        h.tenders().await,
        before,
        "the last verified tenders stay served"
    );
    h.shutdown().await;
}
