//! Live check for probing: prints the connection candidates for each portal URL as JSON.
//!
//! cargo run --example probe -- https://www.tenderned.nl/aankondigingen/overzicht
//!
//! Requests go through the same rate-limited client as the service (one per second per portal).
//! Use `split: development` portals from `testset/portals.yaml`; holdout portals stay unseen.
use reqwest::Url;
use watchtower::{discovery, fetch::Fetcher};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let files = tempfile::tempdir()?;
    let fetcher = Fetcher::new(files.path().into())?;
    let urls: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(!urls.is_empty(), "pass one or more portal URLs");
    for address in urls {
        let url = Url::parse(&address)?;
        let http = fetcher.portal(&url);
        match discovery::probe(&url, &http).await {
            Ok(candidates) => {
                println!("## {address}");
                println!("{}", serde_json::to_string_pretty(&candidates)?);
            }
            Err(failure) => println!("## {address}\nprobe failed: {failure}"),
        }
    }
    Ok(())
}
