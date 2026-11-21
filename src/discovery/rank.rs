use crate::domain::{ConnectionCandidate, ConnectionMethod};
use reqwest::Url;

// Regenerate with scripts/train_ranker.py and the compiled probe example.
// The development run found 11 labelled routes and sampled another 5 directly.
const WEIGHTS: [f64; 13] = [
    -0.243825831563094, // bias
    0.061710500952623,  // HTML
    1.240045113058082,  // JSON
    0.0,                // OCDS
    -0.365486080228832, // feed
    -1.180095365344968, // sitemap
    0.0,                // TED
    -0.243825831563094, // HTTP 2xx
    0.040916351959284,  // log of sample item count
    0.258500350882360,  // field coverage
    1.274795285076197,  // pagination
    0.493033013563669,  // same host
    -0.875813858701351, // explicit machine link
];
const FIELDS: [&str; 6] = [
    "title",
    "deadline",
    "buyer",
    "publicationDate",
    "documents",
    "id",
];

/// Orders candidates for callers that only have the probe result and its start-page candidate.
pub fn rank(candidates: Vec<ConnectionCandidate>) -> Vec<ConnectionCandidate> {
    let start = candidates
        .iter()
        .find(|candidate| candidate.found_via == "start page")
        .and_then(|candidate| Url::parse(&candidate.entry_url).ok());
    score_and_sort(candidates, start.as_ref().and_then(Url::host_str))
}

/// Orders candidates using the registered URL as the origin for the same-host feature.
pub fn rank_for(portal: &Url, candidates: Vec<ConnectionCandidate>) -> Vec<ConnectionCandidate> {
    score_and_sort(candidates, portal.host_str())
}

fn score_and_sort(
    mut candidates: Vec<ConnectionCandidate>,
    portal_host: Option<&str>,
) -> Vec<ConnectionCandidate> {
    for candidate in &mut candidates {
        candidate.score = if candidate.is_usable() {
            let logit: f64 = WEIGHTS
                .into_iter()
                .zip(features(candidate, portal_host))
                .map(|(weight, value)| weight * value)
                .sum();
            (1.0 / (1.0 + (-logit).exp())) as f32
        } else {
            0.0
        };
    }
    candidates.sort_by(|left, right| right.score.total_cmp(&left.score));
    candidates
}

fn features(candidate: &ConnectionCandidate, portal_host: Option<&str>) -> [f64; 13] {
    let entry = Url::parse(&candidate.entry_url).ok();
    let same_host = portal_host.is_some() && portal_host == entry.as_ref().and_then(Url::host_str);
    let coverage = FIELDS
        .iter()
        .filter(|field| {
            candidate.fields_seen.iter().any(|seen| {
                seen.eq_ignore_ascii_case(field)
                    || (**field == "deadline" && seen.eq_ignore_ascii_case("submissionDeadline"))
                    || (**field == "buyer" && seen.eq_ignore_ascii_case("buyerName"))
            })
        })
        .count() as f64
        / FIELDS.len() as f64;
    let explicit_link = ["robots.txt", "link rel=alternate"]
        .iter()
        .any(|source| candidate.found_via.eq_ignore_ascii_case(source));
    use ConnectionMethod::{Feed, Html, Json, Ocds, Sitemap, Ted};
    [
        1.0,
        f64::from(candidate.method == Html),
        f64::from(candidate.method == Json),
        f64::from(candidate.method == Ocds),
        f64::from(candidate.method == Feed),
        f64::from(candidate.method == Sitemap),
        f64::from(candidate.method == Ted),
        f64::from((200..300).contains(&candidate.http_status)),
        (candidate.sample_items as f64).ln_1p(),
        coverage,
        f64::from(candidate.has_pagination),
        f64::from(same_host),
        f64::from(explicit_link),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ConnectionMethod::{Html, Ocds};

    fn candidate(method: ConnectionMethod, path: &str) -> ConnectionCandidate {
        ConnectionCandidate {
            method,
            entry_url: format!("https://portal.example/{path}"),
            found_via: "start page".into(),
            http_status: 200,
            content_type: None,
            sample_items: 0,
            fields_seen: Vec::new(),
            has_pagination: false,
            score: 0.0,
        }
    }

    #[test]
    fn ranks_full_ocds_first_and_refused_candidates_last() {
        let html = candidate(Html, "listing");
        let ocds = ConnectionCandidate {
            sample_items: 20,
            fields_seen: FIELDS.map(str::to_owned).to_vec(),
            has_pagination: true,
            ..candidate(Ocds, "releases")
        };
        let refused = ConnectionCandidate {
            http_status: 403,
            ..ocds.clone()
        };
        let ranked = rank(vec![refused, html, ocds]);
        assert_eq!(ranked[0].method, Ocds);
        assert_eq!(ranked[1].method, Html);
        assert_eq!(ranked[2].http_status, 403);
        assert!(ranked.windows(2).all(|pair| pair[0].score > pair[1].score));
    }

    #[test]
    fn equal_scores_keep_original_order() {
        let ranked = rank(vec![candidate(Html, "first"), candidate(Html, "second")]);
        assert_eq!(ranked[0].score, ranked[1].score);
        assert!(ranked[0].entry_url.ends_with("/first"));
    }
}
