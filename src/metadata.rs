use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone};
use chrono_tz::Tz;
use scraper::{ElementRef, Selector};
use std::collections::BTreeSet;

fn text(element: ElementRef<'_>) -> String {
    element
        .text()
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn definitions(content: ElementRef<'_>, labels: &[&str]) -> Vec<String> {
    let terms = Selector::parse("dl dt").unwrap();
    content
        .select(&terms)
        .filter_map(|term| {
            let label = text(term).trim_end_matches(':').to_lowercase();
            if !labels.contains(&label.as_str()) {
                return None;
            }
            let definition = term.next_siblings().find_map(ElementRef::wrap)?;
            if definition.value().name() != "dd" {
                return None;
            }
            let value = text(definition);
            (!value.is_empty()).then_some(value)
        })
        .collect()
}

pub fn buyer(content: ElementRef<'_>, gov_uk: bool) -> Option<String> {
    let mut labels = vec![
        "buyer",
        "buyer name",
        "contracting authority",
        "procuring entity",
    ];
    if gov_uk {
        labels.push("from");
    }
    unique(definitions(content, &labels).into_iter().collect())
}

pub fn publication_date(content: ElementRef<'_>) -> Option<NaiveDate> {
    unique(
        definitions(content, &["published", "publication date"])
            .into_iter()
            .filter_map(|value| {
                ["%d %B %Y", "%d %b %Y", "%Y-%m-%d"]
                    .iter()
                    .find_map(|format| NaiveDate::parse_from_str(&value, format).ok())
            })
            .collect(),
    )
}

pub fn submission_deadline(content: ElementRef<'_>, zone: Tz) -> Option<DateTime<FixedOffset>> {
    let blocks = Selector::parse("p, dd, td").unwrap();
    let mut deadlines = BTreeSet::new();
    for block in content.select(&blocks) {
        let content = text(block);
        for clause in content.split(['.', ';', '\n']) {
            let lower = clause.to_lowercase();
            if ![
                "deadline for submitting",
                "deadline for submission",
                "deadline for tender",
                "tender submission deadline",
                "submission deadline",
                "closing date and time",
            ]
            .iter()
            .any(|cue| lower.contains(cue))
            {
                continue;
            }
            if let Some(deadline) = local_deadline(clause, zone) {
                deadlines.insert(deadline);
            }
        }
    }
    unique(deadlines)
}

fn local_deadline(clause: &str, zone: Tz) -> Option<DateTime<FixedOffset>> {
    let words: Vec<_> = clause
        .split_whitespace()
        .map(|word| word.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != ':'))
        .collect();
    let dates = words
        .windows(3)
        .filter_map(|parts| {
            let day = parts[0].trim_end_matches(
                ["th", "st", "nd", "rd"]
                    .iter()
                    .find(|suffix| parts[0].ends_with(**suffix))
                    .copied()
                    .unwrap_or(""),
            );
            let date = format!("{day} {} {}", parts[1], parts[2]);
            NaiveDate::parse_from_str(&date, "%d %B %Y")
                .ok()
                .or_else(|| NaiveDate::parse_from_str(&date, "%d %b %Y").ok())
        })
        .collect();
    let date = unique(dates)?;
    let times = words
        .iter()
        .filter_map(|word| NaiveTime::parse_from_str(word, "%H:%M").ok())
        .collect();
    let time = unique(times)?;
    zone.from_local_datetime(&date.and_time(time))
        .single()
        .map(|date| date.fixed_offset())
}

fn unique<T>(set: BTreeSet<T>) -> Option<T> {
    if set.len() == 1 {
        set.into_iter().next()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_labeled_buyer_and_a_british_summer_time_deadline() {
        let html = scraper::Html::parse_fragment(
            r#"
            <dl><dt>From:</dt><dd><a href="/government/organisations/legal-aid-agency">Legal Aid Agency</a></dd>
            <dt>Published:</dt><dd>28 July 2026</dd><dt>Last updated:</dt><dd>29 October 2026</dd></dl>
            <p>The deadline for submitting a tender through this procurement process is 17:00 on 31st March 2027.</p>
            <p>Tenders received by 17:00 on 4 September 2026 will be assessed for service launch.</p>
        "#,
        );
        assert_eq!(
            buyer(html.root_element(), true).as_deref(),
            Some("Legal Aid Agency")
        );
        assert_eq!(buyer(html.root_element(), false), None);
        assert_eq!(
            publication_date(html.root_element()),
            NaiveDate::from_ymd_opt(2026, 7, 28)
        );
        assert_eq!(
            submission_deadline(html.root_element(), chrono_tz::Europe::London)
                .unwrap()
                .to_rfc3339(),
            "2027-03-31T17:00:00+01:00"
        );
    }

    #[test]
    fn leaves_ambiguous_or_date_only_deadlines_unset() {
        for source in [
            "<p>The deadline for submission is 31 March 2027.</p>",
            "<p>The deadline for submission is 17:00 on 31 March 2027.</p><p>The submission deadline is 14:00 on 30 March 2027.</p>",
        ] {
            let html = scraper::Html::parse_fragment(source);
            assert!(submission_deadline(html.root_element(), chrono_tz::Europe::London).is_none());
        }
    }
}
