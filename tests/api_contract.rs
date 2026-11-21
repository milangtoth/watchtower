//! The challenge appendix's contract for every route: JSON answers including errors, one ID per
//! address, `changedSince` with either offset encoding, exact entry shapes and offsets on every
//! time.

mod common;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use chrono::DateTime;
use common::*;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Asserts a JSON error body `{"detail": "…"}` with the given status.
fn assert_error((status, body): (StatusCode, Vec<u8>), expected: StatusCode, what: &str) {
    assert_eq!(status, expected, "{what}");
    let body: Value = serde_json::from_slice(&body)
        .unwrap_or_else(|_| panic!("{what}: body is not JSON: {body:?}"));
    assert!(body["detail"].is_string(), "{what}: {body}");
}

async fn raw(app: &axum::Router, method: &str, path: &str, body: &str) -> (StatusCode, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    (status, body.to_vec())
}

fn assert_offset(value: &Value, what: &str) {
    let text = value
        .as_str()
        .unwrap_or_else(|| panic!("{what} is {value}"));
    DateTime::parse_from_rfc3339(text)
        .unwrap_or_else(|error| panic!("{what} {text} has no offset: {error}"));
}

#[tokio::test]
async fn one_address_has_one_id_however_it_is_written() {
    let h = Harness::attach(Layout::one_page(), html_recipe(), "/").await;
    let url = h.portal().await["url"].as_str().unwrap().to_owned();
    let (scheme, rest) = url.split_once("://").unwrap();
    let variants = [
        url.clone(),
        format!("  {url}\n"),
        format!("{url}#tenders"),
        format!("{url}?"),
        format!("{}://{rest}", scheme.to_uppercase()),
    ];
    for variant in variants {
        let (status, body) =
            call(&h.app, "POST", "/portals", Some(json!({ "url": variant }))).await;
        assert_eq!(status, StatusCode::CREATED, "{variant:?}");
        let id: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(id["id"], h.portal.as_str(), "{variant:?}");
    }
    let portals = get_json(&h.app, "/portals").await;
    assert_eq!(portals.as_array().unwrap().len(), 1);
    assert_eq!(portals[0]["url"], url, "the first address is kept as sent");
    h.shutdown().await;
}

#[tokio::test]
async fn every_error_is_json() {
    let h = Harness::attach(Layout::one_page(), html_recipe(), "/").await;
    let app = &h.app;
    assert_error(
        raw(app, "POST", "/portals", "{not json").await,
        StatusCode::BAD_REQUEST,
        "invalid JSON",
    );
    assert_error(
        call(app, "POST", "/portals", Some(json!({}))).await,
        StatusCode::BAD_REQUEST,
        "missing url",
    );
    for url in [
        "ftp://example.com/",
        "https://user:secret@example.com/",
        "tenders",
    ] {
        assert_error(
            call(app, "POST", "/portals", Some(json!({ "url": url }))).await,
            StatusCode::BAD_REQUEST,
            url,
        );
    }

    let unknown = [
        ("GET", "/portals/unknown".to_owned()),
        ("DELETE", "/portals/unknown".to_owned()),
        ("POST", "/portals/unknown/refresh".to_owned()),
        ("GET", "/portals/unknown/tenders".to_owned()),
        ("GET", "/portals/unknown/runs".to_owned()),
        ("GET", format!("/files/{}", "0".repeat(64))),
        ("GET", "/files/not-a-hash".to_owned()),
        ("GET", "/no/such/route".to_owned()),
    ];
    for (method, path) in unknown {
        assert_error(
            call(app, method, &path, None).await,
            StatusCode::NOT_FOUND,
            &format!("{method} {path}"),
        );
    }

    let portal = format!("/portals/{}", h.portal);
    for (method, path) in [
        ("PUT", portal.clone()),
        ("PATCH", "/portals".to_owned()),
        ("GET", format!("{portal}/refresh")),
    ] {
        assert_error(
            call(app, method, &path, None).await,
            StatusCode::METHOD_NOT_ALLOWED,
            &format!("{method} {path}"),
        );
    }
    h.shutdown().await;
}

#[tokio::test]
async fn changed_since_accepts_a_plus_offset_with_or_without_encoding() {
    let h = Harness::attach(Layout::one_page(), html_recipe(), "/").await;
    let tenders = format!("/portals/{}/tenders", h.portal);
    let all = h.tenders().await;
    assert_eq!(all.as_array().unwrap().len(), 2);
    for since in ["2000-01-01T00:00:00+01:00", "2000-01-01T00:00:00%2B01:00"] {
        assert_eq!(
            get_json(&h.app, &format!("{tenders}?changedSince={since}")).await,
            all,
            "{since}"
        );
    }
    assert_eq!(
        get_json(
            &h.app,
            &format!("{tenders}?changedSince=2999-01-01T00:00:00+01:00")
        )
        .await,
        json!([])
    );
    for since in ["yesterday", "2026-12-09", "2026-12-09T10:00:00"] {
        assert_error(
            call(
                &h.app,
                "GET",
                &format!("{tenders}?changedSince={since}"),
                None,
            )
            .await,
            StatusCode::BAD_REQUEST,
            since,
        );
    }
    h.shutdown().await;
}

#[tokio::test]
async fn entries_have_the_appendix_shape_and_every_time_an_offset() {
    let h = Harness::attach(Layout::one_page(), html_recipe(), "/").await;

    let portals = get_json(&h.app, "/portals").await;
    let entry = &portals[0];
    let mut keys: Vec<_> = entry.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "id",
            "lastSuccessAt",
            "reason",
            "state",
            "tenderCount",
            "url"
        ]
    );
    assert_eq!(entry, &h.portal().await);
    assert_eq!(entry["state"], "serving");
    assert_offset(&entry["lastSuccessAt"], "lastSuccessAt");

    for run in h.runs().await {
        assert_offset(&run["startedAt"], "startedAt");
        assert_offset(&run["finishedAt"], "finishedAt");
    }
    for tender in h.tenders().await.as_array().unwrap() {
        assert_offset(&tender["lastChangedAt"], "lastChangedAt");
        assert_offset(&tender["submissionDeadline"], "submissionDeadline");
    }

    let path = format!("/portals/{}", h.portal);
    assert_eq!(
        call(&h.app, "DELETE", &path, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(get_json(&h.app, "/portals").await, json!([]));
    for path in [
        path.clone(),
        format!("{path}/tenders"),
        format!("{path}/runs"),
    ] {
        assert_error(
            call(&h.app, "GET", &path, None).await,
            StatusCode::NOT_FOUND,
            &path,
        );
    }
    assert_error(
        call(&h.app, "DELETE", &path, None).await,
        StatusCode::NOT_FOUND,
        "deleting twice",
    );
    h.shutdown().await;
}
