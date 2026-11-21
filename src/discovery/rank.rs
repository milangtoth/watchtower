use crate::{
    domain::{ConnectionCandidate, ConnectionMethod},
    recipe::date_filters,
};
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
    // A windowed OCDS entry goes before the same entry without its date filters, which would
    // page through the API's whole history.
    for index in 0..candidates.len() {
        if let Some(unfiltered) = unfiltered_twin_score(&candidates, index) {
            let windowed = &mut candidates[index].score;
            *windowed = windowed.max(unfiltered.next_up());
        }
    }
    candidates.sort_by(|left, right| right.score.total_cmp(&left.score));
    candidates
}

/// The highest score of a usable OCDS candidate whose entry URL is candidate `index`'s entry
/// URL without its date filters, when `index` is a usable OCDS candidate with date filters.
fn unfiltered_twin_score(candidates: &[ConnectionCandidate], index: usize) -> Option<f32> {
    let windowed = &candidates[index];
    if windowed.method != ConnectionMethod::Ocds || !windowed.is_usable() {
        return None;
    }
    let url = Url::parse(&windowed.entry_url).ok()?;
    let filters = date_filters(&url);
    if filters.is_empty() {
        return None;
    }
    let shape = |url: &Url| {
        let mut pairs: Vec<(String, String)> = url
            .query_pairs()
            .into_owned()
            .filter(|(key, _)| !filters.iter().any(|filter| filter.key == *key))
            .collect();
        pairs.sort();
        (url.origin(), url.path().to_owned(), pairs)
    };
    let target = shape(&url);
    candidates
        .iter()
        .filter(|other| other.method == ConnectionMethod::Ocds && other.is_usable())
        .filter_map(|other| Some((other, Url::parse(&other.entry_url).ok()?)))
        .filter(|(_, other)| date_filters(other).is_empty() && shape(other) == target)
        .map(|(other, _)| other.score)
        .max_by(f32::total_cmp)
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
    fn a_windowed_ocds_entry_outranks_its_unfiltered_twin() {
        let unfiltered = ConnectionCandidate {
            sample_items: 100,
            has_pagination: true,
            ..candidate(Ocds, "api/releases?stages=tender")
        };
        let windowed = ConnectionCandidate {
            sample_items: 3,
            ..candidate(
                Ocds,
                "api/releases?stages=tender&updatedFrom=2026-08-22&updatedTo=2026-11-20",
            )
        };
        let other_path = ConnectionCandidate {
            sample_items: 100,
            has_pagination: true,
            ..candidate(Ocds, "api/records")
        };
        let ranked = rank(vec![other_path, unfiltered.clone(), windowed.clone()]);
        let at = |url: &str| ranked.iter().position(|c| c.entry_url == url).unwrap();
        assert!(at(&windowed.entry_url) < at(&unfiltered.entry_url));
        assert!(ranked[at(&windowed.entry_url)].score > ranked[at(&unfiltered.entry_url)].score);

        // Without its twin the windowed entry keeps its own score.
        let alone = rank(vec![windowed.clone(), candidate(Html, "listing")]);
        let plain = rank(vec![ConnectionCandidate {
            entry_url: "https://portal.example/api/other?updatedFrom=2026-08-22".into(),
            ..windowed
        }]);
        let alone = alone.iter().find(|c| c.method == Ocds).unwrap();
        assert_eq!(alone.score, plain[0].score);
    }

    #[test]
    fn equal_scores_keep_original_order() {
        let ranked = rank(vec![candidate(Html, "first"), candidate(Html, "second")]);
        assert_eq!(ranked[0].score, ranked[1].score);
        assert!(ranked[0].entry_url.ends_with("/first"));
    }
}
