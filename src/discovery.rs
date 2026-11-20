//! Finding and ordering the ways a portal can be read, before any model call.
//!
//! `probe` (owned by David, in `discovery/probe.rs`) makes cheap requests through the portal's
//! rate-limited client and reports candidate connection methods. `rank` (owned by qwas) orders
//! them with an embedded scorer; until it lands it is a placeholder that keeps the probe's order.

use crate::domain::ConnectionCandidate;

mod probe;

pub use probe::probe;

/// Orders candidates from most to least promising and sets each `score` between 0 and 1.
///
/// Placeholder: keeps the probe's candidates, ordered by their existing score.
pub fn rank(mut candidates: Vec<ConnectionCandidate>) -> Vec<ConnectionCandidate> {
    candidates.sort_by(|left, right| right.score.total_cmp(&left.score));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{ConnectionDecision, ConnectionMethod},
        model::top_ranked,
    };

    fn candidate(method: ConnectionMethod, score: f32) -> ConnectionCandidate {
        ConnectionCandidate {
            method,
            entry_url: format!("https://portal.example/{}", method.label()),
            found_via: "start page".into(),
            http_status: 200,
            content_type: Some("text/html".into()),
            sample_items: 5,
            fields_seen: vec!["title".into()],
            has_pagination: true,
            score,
        }
    }

    #[test]
    fn rank_orders_by_score_and_decision_keeps_the_rest_as_fallbacks() {
        let ranked = rank(vec![
            candidate(ConnectionMethod::Html, 0.2),
            candidate(ConnectionMethod::Ocds, 0.9),
            candidate(ConnectionMethod::Feed, 0.5),
        ]);
        let decision = top_ranked(ranked, 1).unwrap();
        assert_eq!(decision.chosen.method, ConnectionMethod::Ocds);
        let fallbacks: Vec<_> = decision.alternatives.iter().map(|c| c.method).collect();
        assert_eq!(fallbacks, [ConnectionMethod::Feed, ConnectionMethod::Html]);
        assert!(top_ranked(Vec::new(), 1).is_err());
    }

    #[test]
    fn decisions_serialize_with_camel_case_keys_and_lowercase_methods() {
        let decision = top_ranked(vec![candidate(ConnectionMethod::Sitemap, 0.7)], 2).unwrap();
        let value = serde_json::to_value(&decision).unwrap();
        for key in [
            "chosen",
            "alternatives",
            "rationale",
            "model",
            "decidedAt",
            "attempt",
        ] {
            assert!(value.get(key).is_some(), "missing {key}");
        }
        let chosen = &value["chosen"];
        assert_eq!(chosen["method"], "sitemap");
        for key in [
            "entryUrl",
            "foundVia",
            "httpStatus",
            "contentType",
            "sampleItems",
            "fieldsSeen",
            "hasPagination",
            "score",
        ] {
            assert!(chosen.get(key).is_some(), "missing {key}");
        }
        let back: ConnectionDecision = serde_json::from_value(value).unwrap();
        assert_eq!(back.attempt, 2);
    }
}
