//! Live check for probing: prints one JSON document with the candidates for each portal URL.
//!
//! cargo run --example probe -- https://www.tenderned.nl/aankondigingen/overzicht > probe.json
//!
//! Output is a JSON array with one object per URL, in argument order:
//! `{"url": ..., "candidates": [...]}` or, when probing fails, `{"url": ..., "error": ...}`.
//! Candidates use the same camelCase shape as stored connection decisions.
//!
//! Requests go through the same rate-limited client as the service (one per second per portal).
//! Use `split: development` portals from `testset/portals.yaml`; holdout portals stay unseen.
use reqwest::Url;
use serde_json::{Value, json};
use watchtower::{discovery, fetch::Fetcher};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let urls: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(!urls.is_empty(), "pass one or more portal URLs");
    let files = tempfile::tempdir()?;
    let fetcher = Fetcher::new(files.path().into())?;
    let mut results = Vec::new();
    for address in urls {
        let result: Value = match Url::parse(&address) {
            Err(error) => json!({ "url": address, "error": format!("invalid URL: {error}") }),
            Ok(url) => match discovery::probe(&url, &fetcher.portal(&url)).await {
                Ok(candidates) => json!({ "url": address, "candidates": candidates }),
                Err(failure) => json!({ "url": address, "error": failure.to_string() }),
            },
        };
        results.push(result);
    }
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}
