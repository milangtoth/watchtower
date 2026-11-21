//! Recovery without a person: temporary outages, throttling, exhausted repairs and access
//! blocks. The recovery policy runs on seconds here; production uses `Recovery::default()`.

mod common;

use common::*;
use std::{sync::atomic::Ordering, time::Duration};
use watchtower::worker::Recovery;

fn fast() -> Recovery {
    Recovery {
        backoff: [1, 2, 3],
        retry_after_cap: 4,
        outage_failures: 3,
        outage_seconds: 3600,
        repair_cooldown: 3600,
        recheck: 2,
    }
}

fn state(portal: &serde_json::Value) -> &str {
    portal["state"].as_str().unwrap()
}

fn reason(portal: &serde_json::Value) -> &str {
    portal["reason"].as_str().unwrap()
}

#[tokio::test]
async fn outage_keeps_serving_and_recovers_without_a_refresh() {
    let h = Harness::attach_with(Layout::two_pages(), html_recipe(), "/", 3600, fast()).await;
    let before = h.tenders().await;
    h.site.lock().unwrap().listing_status = Some((503, None));
    let run = h.refresh().await;
    h.settle(Some(&run)).await;
    h.observe("outage").await;
    let portal = h.portal().await;
    assert_eq!(state(&portal), "serving");
    assert!(
        reason(&portal).starts_with("Last read failed temporarily: Portal returned HTTP 503"),
        "{portal}"
    );
    assert_eq!(h.tenders().await, before);
    let runs = h.runs().await;
    assert_eq!(runs.last().unwrap()["result"], "failed");

    h.site.lock().unwrap().listing_status = None;
    h.wait_for(15, |portal| reason(portal) == "Last read verified")
        .await;
    h.observe("outage-recovered").await;
    assert!(h.runs().await.len() > runs.len(), "a scheduled read ran");
    assert_eq!(h.tenders().await, before);
    assert_eq!(h.model.learns.load(Ordering::SeqCst), 1);
    assert_eq!(h.model.verifies.load(Ordering::SeqCst), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn throttled_read_waits_for_the_capped_retry_after() {
    let h = Harness::attach_with(Layout::two_pages(), html_recipe(), "/", 3600, fast()).await;
    let hits = h.site.lock().unwrap().listing_hits.len();
    h.site.lock().unwrap().listing_status = Some((429, Some("100")));
    let run = h.refresh().await;
    h.settle(Some(&run)).await;
    let portal = h.portal().await;
    assert_eq!(state(&portal), "serving");
    assert!(reason(&portal).contains("asked to wait 100s"), "{portal}");
    assert_eq!(
        h.site.lock().unwrap().listing_hits.len(),
        hits + 1,
        "a throttled read is not retried at once"
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        while h.site.lock().unwrap().listing_hits.len() < hits + 2 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the throttled portal was not read again");
    let gap = {
        let layout = h.site.lock().unwrap();
        layout.listing_hits[hits + 1] - layout.listing_hits[hits]
    };
    h.observe("throttled").await;
    assert!(
        gap >= Duration::from_secs(3) && gap < Duration::from_secs(8),
        "the retry waited {gap:?}, not the 4s cap"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn prolonged_outage_becomes_failed_and_still_recovers() {
    let h = Harness::attach_with(Layout::two_pages(), html_recipe(), "/", 3600, fast()).await;
    let before = h.tenders().await;
    h.site.lock().unwrap().listing_status = Some((503, None));
    h.refresh().await;
    let failed = h.wait_for(40, |portal| state(portal) == "failed").await;
    h.observe("prolonged-outage").await;
    assert!(
        reason(&failed)
            .starts_with("Portal unavailable after 3 attempt(s): Portal returned HTTP 503"),
        "{failed}"
    );
    assert_eq!(
        h.tenders().await,
        before,
        "the last verified tenders stay served"
    );

    h.site.lock().unwrap().listing_status = None;
    h.wait_for(20, |portal| state(portal) == "serving").await;
    h.observe("prolonged-outage-recovered").await;
    assert_eq!(h.model.learns.load(Ordering::SeqCst), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn exhausted_repair_keeps_reading_but_repairs_wait_for_the_cooldown() {
    let h = Harness::attach_with(Layout::two_pages(), html_recipe(), "/", 2, fast()).await;
    let before = h.tenders().await;
    h.change(|layout| layout.next_class = "pager-next").await;
    let portal = h.portal().await;
    assert_eq!(state(&portal), "failed");
    assert!(reason(&portal).starts_with("Repair did not restore delivery"));
    let learns = h.model.learns.load(Ordering::SeqCst);
    let runs = h.runs().await.len();

    let waiting = h
        .wait_for(20, |portal| {
            reason(portal).contains("the next repair starts after")
        })
        .await;
    h.observe("repair-cooldown").await;
    assert_eq!(state(&waiting), "failed");
    assert!(h.runs().await.len() > runs, "scheduled reads continue");
    assert_eq!(
        h.model.learns.load(Ordering::SeqCst),
        learns,
        "no paid repair inside the cooldown"
    );
    assert_eq!(h.tenders().await, before);

    h.change(|_| {}).await;
    assert!(
        h.model.learns.load(Ordering::SeqCst) > learns,
        "a manual refresh repairs at once"
    );
    h.observe("repair-after-manual-refresh").await;

    h.site.lock().unwrap().next_class = "next";
    h.wait_for(20, |portal| state(portal) == "serving").await;
    h.observe("repair-cooldown-recovered").await;
    h.shutdown().await;
}

#[tokio::test]
async fn blocked_portal_is_rechecked_and_recovers() {
    let h = Harness::attach_with(Layout::two_pages(), html_recipe(), "/", 3600, fast()).await;
    let before = h.tenders().await;
    h.change(|layout| layout.login_wall = true).await;
    let portal = h.portal().await;
    assert_eq!(state(&portal), "failed");
    assert!(reason(&portal).contains("login"), "{portal}");
    assert_eq!(h.tenders().await, before);

    h.site.lock().unwrap().login_wall = false;
    h.wait_for(15, |portal| state(portal) == "serving").await;
    h.observe("block-lifted").await;
    assert_eq!(h.model.learns.load(Ordering::SeqCst), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn outage_during_attach_stays_integrating_then_attaches() {
    let mut layout = Layout::two_pages();
    layout.listing_status = Some((503, None));
    let h = Harness::attach_with(layout, html_recipe(), "/", 3600, fast()).await;
    let portal = h.portal().await;
    h.observe("attach-outage").await;
    assert_eq!(state(&portal), "integrating");
    assert!(
        reason(&portal).starts_with("Attach paused by a temporary failure"),
        "{portal}"
    );
    assert_eq!(portal["lastSuccessAt"], serde_json::Value::Null);

    h.site.lock().unwrap().listing_status = None;
    h.wait_for(15, |portal| state(portal) == "serving").await;
    h.observe("attach-outage-recovered").await;
    assert_eq!(ids(&h.tenders().await), ["A", "B"]);
    h.shutdown().await;
}
