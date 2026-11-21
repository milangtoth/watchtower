//! Finding and ordering the ways a portal can be read, before any model call.
//!
//! The probe uses the portal's rate-limited client and the ranker scores its candidate evidence.

mod probe;
mod rank;

pub use probe::probe;
pub use rank::{rank, rank_for};

#[cfg(test)]
mod tests {
    use crate::{
        domain::{ConnectionCandidate, ConnectionDecision, ConnectionMethod},
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
    fn decision_keeps_the_rest_as_fallbacks() {
        let ranked = vec![
            candidate(ConnectionMethod::Ocds, 0.9),
            candidate(ConnectionMethod::Feed, 0.5),
            candidate(ConnectionMethod::Html, 0.2),
        ];
        let decision = top_ranked(ranked, 1).unwrap();
        assert_eq!(decision.chosen.method, ConnectionMethod::Ocds);
        let fallbacks: Vec<_> = decision.alternatives.iter().map(|c| c.method).collect();
        assert_eq!(fallbacks, [ConnectionMethod::Feed, ConnectionMethod::Html]);
        assert!(top_ranked(Vec::new(), 1).is_err());
    }

    #[test]
    fn ted_is_never_chosen_but_stays_in_the_evidence() {
        // The probe reports a TED candidate without a sampled response.
        let ted = ConnectionCandidate {
            http_status: 0,
            sample_items: 40,
            fields_seen: vec!["title".into(), "deadline".into(), "buyer".into()],
            ..candidate(ConnectionMethod::Ted, 0.0)
        };
        let answered_ted = ConnectionCandidate {
            http_status: 200,
            ..ted.clone()
        };
        assert!(!ted.is_usable() && !answered_ted.is_usable());

        let ranked = super::rank(vec![
            ted.clone(),
            candidate(ConnectionMethod::Html, 0.0),
            answered_ted,
        ]);
        assert_eq!(ranked[0].method, ConnectionMethod::Html);
        assert!(
            ranked
                .iter()
                .filter(|c| c.method == ConnectionMethod::Ted)
                .all(|c| c.score == 0.0)
        );

        // Even listed first, TED is skipped for the first usable candidate and kept as evidence.
        let decision =
            top_ranked(vec![ted.clone(), candidate(ConnectionMethod::Feed, 0.4)], 1).unwrap();
        assert_eq!(decision.chosen.method, ConnectionMethod::Feed);
        assert_eq!(decision.alternatives[0].method, ConnectionMethod::Ted);
        assert!(
            !decision
                .alternatives
                .iter()
                .any(ConnectionCandidate::is_usable)
        );
        assert!(
            top_ranked(vec![ted], 1).is_err(),
            "TED alone cannot be read"
        );
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
