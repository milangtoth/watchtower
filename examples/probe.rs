//! Live check for probing. Pass --json for one machine-readable result per URL.
//! With --json, --rank or --decide enables the later stages.
//!
//! cargo run --example probe -- https://www.tenderned.nl/aankondigingen/overzicht
//!
//! Requests go through the same rate-limited client as the service (one per second per portal).
//! Use `split: development` portals from `testset/portals.yaml`; holdout portals stay unseen.
use reqwest::Url;
use serde_json::json;
use watchtower::{
    db::Store,
    discovery,
    domain::{ConnectionCandidate, ModelOperation, ReadFailure},
    fetch::Fetcher,
    model::{OpenAi, PortalModel, ReasoningContext},
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let files = tempfile::tempdir()?;
    let fetcher = Fetcher::new(files.path().into())?;
    let mut args = std::env::args().skip(1).peekable();
    let machine_readable = args.peek().is_some_and(|arg| arg == "--json");
    if machine_readable {
        args.next();
    }
    let stage =
        args.next_if(|arg| machine_readable && matches!(arg.as_str(), "--rank" | "--decide"));
    let model = if stage.as_deref() == Some("--decide") {
        for variable in ["OPENAI_API_KEY", "WATCHTOWER_MODEL"] {
            anyhow::ensure!(
                std::env::var(variable).is_ok_and(|value| !value.trim().is_empty()),
                "Set {variable} to evaluate model decisions"
            );
        }
        let store = Store::open(&files.path().join("evaluation.sqlite")).await?;
        Some((OpenAi::new(store.clone())?, store))
    } else {
        None
    };
    let urls: Vec<_> = args.collect();
    anyhow::ensure!(!urls.is_empty(), "pass one or more portal URLs");
    for address in urls {
        let url = Url::parse(&address)?;
        let http = fetcher.portal(&url);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            discovery::probe(&url, &http),
        )
        .await
        .unwrap_or_else(|_| Err(ReadFailure::Temporary("Probe sample timed out".into())));
        match result {
            Ok(candidates) if machine_readable => {
                let candidates = if stage.is_some() {
                    discovery::rank_for(&url, candidates)
                } else {
                    candidates
                };
                let mut result = json!({ "url": address, "candidates": candidates });
                if let Some((model, store)) = &model {
                    store.register(&address).await?;
                    let job = store.claim().await?.expect("a new evaluation job");
                    let context = ReasoningContext {
                        job: &job,
                        url: &url,
                        http: &http,
                    };
                    let calls = usize::from(candidates.iter().any(ConnectionCandidate::is_usable));
                    match model.decide(&context, candidates).await {
                        Ok(decision) => result["decision"] = json!(decision),
                        Err(failure) => result["decisionFailure"] = json!(failure.to_string()),
                    }
                    let (_, usage) = store.usage_totals(&job, ModelOperation::Decide).await?;
                    result["usage"] = json!({
                        "calls": calls, "inputTokens": usage.input,
                        "cachedInputTokens": usage.cached_input, "outputTokens": usage.output
                    });
                }
                println!("{result}");
            }
            Err(failure) if machine_readable => {
                let kind = match failure {
                    ReadFailure::Blocked(_) => "blocked",
                    ReadFailure::Incomplete(_) => "incomplete",
                    ReadFailure::Unsupported(_) => "unsupported",
                    ReadFailure::Temporary(_) => "temporary",
                    ReadFailure::Throttled { .. } => "throttled",
                };
                println!(
                    "{}",
                    json!({
                        "url": address, "failure": {"kind": kind, "reason": failure.to_string()}
                    })
                );
            }
            Ok(candidates) => {
                println!("## {address}");
                println!("{}", serde_json::to_string_pretty(&candidates)?);
            }
            Err(failure) => println!("## {address}\nprobe failed: {failure}"),
        }
    }
    Ok(())
}
