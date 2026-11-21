use crate::{api::Api, domain::RunHistory};
use axum::{
    Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse},
    routing::get,
};
use std::path::PathBuf;

const JAVASCRIPT: [(header::HeaderName, &str); 1] =
    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")];

/// Run groups the dashboard shows, newest first. A broken read and its repair count as one.
const HISTORY_GROUPS: usize = 8;

pub(crate) fn routes() -> Router<Api> {
    Router::new()
        .route(
            "/dashboard/assistant.js",
            get(|| async { (JAVASCRIPT, include_str!("dashboard/assistant.js")) }),
        )
        .route(
            "/dashboard/tender-search.js",
            get(|| async { (JAVASCRIPT, include_str!("dashboard/tender-search.js")) }),
        )
        .route("/dashboard/elevenlabs-sdk.js", get(voice_sdk))
        .route("/dashboard/portals/{id}/history", get(history))
}

async fn voice_sdk() -> Result<impl IntoResponse, StatusCode> {
    let directory = std::env::var_os("WATCHTOWER_DASHBOARD_ASSETS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("src/dashboard"));
    let content = tokio::fs::read(directory.join("elevenlabs-sdk.js"))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok((JAVASCRIPT, content))
}

/// The portal's run history with each run's reason, as an HTML fragment for the dashboard.
/// Dashboard only: `GET /portals/{id}/runs` keeps the appendix shape without reasons.
async fn history(
    State(api): State<Api>,
    Path(id): Path<String>,
) -> Result<Html<String>, StatusCode> {
    let runs = api
        .store
        .run_history(&id)
        .await
        .map_err(|error| {
            tracing::error!(%error, portal_id = %id, "Could not load run history");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Html(render_history(&runs)))
}

/// Renders runs newest first. A read that failed and the repair it started share one entry,
/// so what broke reads directly above how it was repaired.
pub fn render_history(runs: &[RunHistory]) -> String {
    if runs.is_empty() {
        return "<p>Waiting for first read</p>".to_owned();
    }
    let mut groups: Vec<Vec<&RunHistory>> = Vec::new();
    for run in runs {
        let pairs = run.kind == "repair"
            && groups.last().is_some_and(|group| {
                matches!(group.as_slice(), [read] if read.kind == "read" && read.result.as_deref() == Some("failed"))
            });
        match groups.last_mut() {
            Some(group) if pairs => group.push(run),
            _ => groups.push(vec![run]),
        }
    }
    let mut html = String::from(r#"<ul class="record-list run-history">"#);
    for group in groups.iter().rev().take(HISTORY_GROUPS) {
        if let [read, repair] = group.as_slice() {
            html.push_str(r#"<li class="run-pair">"#);
            step(&mut html, read, "what broke");
            let label = match repair.result.as_deref() {
                Some("ok") => "how it was repaired",
                Some(_) => "why the repair did not restore delivery",
                None => "repair in progress",
            };
            step(&mut html, repair, label);
            html.push_str("</li>");
        } else {
            html.push_str("<li>");
            step(&mut html, group[0], "");
            html.push_str("</li>");
        }
    }
    html.push_str("</ul>");
    html
}

fn step(html: &mut String, run: &RunHistory, role: &str) {
    let result = run.result.as_deref().unwrap_or("running");
    let mut name = format!("{} · {result}", run.kind);
    if !role.is_empty() {
        name.push_str(" · ");
        name.push_str(role);
    }
    html.push_str(&format!(
        r#"<div class="run-step" data-kind="{kind}" data-result="{result}"><span class="record-name">{name}</span><time class="record-time" datetime="{time}">{time}</time>"#,
        kind = escape(&run.kind),
        result = escape(result),
        name = escape(&name),
        time = escape(&run.started_at),
    ));
    if let Some(reason) = &run.reason {
        html.push_str(&format!(r#"<p class="run-reason">{}</p>"#, escape(reason)));
    }
    html.push_str("</div>");
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, kind: &str, result: Option<&str>, reason: Option<&str>) -> RunHistory {
        RunHistory {
            id: id.into(),
            kind: kind.into(),
            started_at: format!("2026-11-20T10:0{id}:00+00:00"),
            finished_at: None,
            result: result.map(Into::into),
            reason: reason.map(Into::into),
        }
    }

    #[test]
    fn a_failed_read_and_its_repair_render_as_one_entry_newest_first() {
        let html = render_history(&[
            run("1", "read", Some("ok"), Some("Attached and verified")),
            run(
                "2",
                "read",
                Some("failed"),
                Some("Listing selector <.row> found 0 rows"),
            ),
            run(
                "3",
                "repair",
                Some("ok"),
                Some("Repaired via HTML: changed items"),
            ),
            run("4", "read", Some("ok"), Some("Read verified")),
        ]);
        let pair = html
            .find(r#"<li class="run-pair">"#)
            .expect("a paired entry");
        let broke = html.find("read · failed · what broke").unwrap();
        let fixed = html.find("repair · ok · how it was repaired").unwrap();
        assert!(pair < broke && broke < fixed);
        assert!(html.find("Read verified").unwrap() < pair, "newest first");
        assert!(html.contains("Listing selector &lt;.row&gt; found 0 rows"));
        assert_eq!(html.matches("<li").count(), 3);
    }

    #[test]
    fn a_repair_after_a_healthy_read_stays_on_its_own() {
        let html = render_history(&[
            run("1", "read", Some("ok"), None),
            run("2", "repair", None, None),
        ]);
        assert!(!html.contains("run-pair"));
        assert!(html.contains("repair · running"));
        assert_eq!(render_history(&[]), "<p>Waiting for first read</p>");
    }
}
