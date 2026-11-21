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

/// Labels that name the buyer on most portals. A recipe adds portal wording in `buyer_labels`.
const BUYER_LABELS: &[&str] = &[
    "buyer",
    "buyer name",
    "contracting authority",
    "procuring entity",
];

/// The buyer named in a definition list under a common label or one of `extra` (lowercase).
pub fn buyer(content: ElementRef<'_>, extra: &[String]) -> Option<String> {
    let mut labels: Vec<&str> = BUYER_LABELS.to_vec();
    labels.extend(extra.iter().map(String::as_str));
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

/// Wording that introduces the deadline for submitting tenders, in several languages.
const DEADLINE_CUES: &[&str] = &[
    "deadline for submitting",
    "deadline for submission",
    "deadline for the submission",
    "deadline for tender",
    "submission deadline",
    "tender deadline",
    "closing date",
    "submitted by",
    "submitted no later than",
    "submit by",
    "no later than",
    "angebotsfrist",
    "einreichungsfrist",
    "schlusstermin",
    "sluitingsdatum",
    "uiterste datum",
    "date limite",
    "remise des offres",
    "fecha límite",
    "plazo de presentación",
    "termin składania",
    "lhůta pro podání",
];

/// Wording for other dates near a deadline: questions, expressions of interest, or a single
/// stage, lot or assessment round. Such a sentence does not state the tender's deadline.
const NOT_THE_DEADLINE: &[&str] = &[
    "question",
    "clarification",
    "enquir",
    "inquir",
    "expression of interest",
    "expressions of interest",
    "stage",
    "phase",
    " round ",
    " lot ",
    "assessed",
    "fragen",
    "vragen",
];

/// The tender's submission deadline stated in the page text. Only a sentence that introduces
/// the deadline and gives exactly one date and one time counts, read in `zone`. When the page
/// states different deadlines, none is returned: a wrong deadline is worse than a missing one.
pub fn submission_deadline(content: ElementRef<'_>, zone: Tz) -> Option<DateTime<FixedOffset>> {
    let blocks = Selector::parse("p, dd, td, li").unwrap();
    let mut deadlines = BTreeSet::new();
    for block in content.select(&blocks) {
        let content = text(block);
        for clause in clauses(&content) {
            let lower = format!(" {} ", clause.to_lowercase());
            if !DEADLINE_CUES.iter().any(|cue| lower.contains(cue))
                || NOT_THE_DEADLINE.iter().any(|word| lower.contains(word))
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

/// Sentences and list parts of a text block. A period ends a sentence only before a space or
/// the end, so "31.12.2025" and "17.00" stay whole.
fn clauses(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    for piece in text.split([';', '\n']) {
        let mut start = 0;
        let bytes = piece.as_bytes();
        for (index, ch) in piece.char_indices() {
            if ch == '.'
                && bytes
                    .get(index + 1)
                    .is_none_or(|next| next.is_ascii_whitespace())
            {
                parts.push(&piece[start..index]);
                start = index + 1;
            }
        }
        parts.push(&piece[start..]);
    }
    parts.retain(|part| !part.trim().is_empty());
    parts
}

fn local_deadline(clause: &str, zone: Tz) -> Option<DateTime<FixedOffset>> {
    let words: Vec<_> = clause
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|ch: char| {
                !ch.is_alphanumeric() && !matches!(ch, ':' | '.' | '/' | '-')
            })
            .trim_end_matches(['.', '-', '/'])
        })
        .filter(|word| !word.is_empty())
        .collect();
    let mut dates: BTreeSet<NaiveDate> = words
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
    dates.extend(words.iter().filter_map(|word| {
        ["%d.%m.%Y", "%d/%m/%Y", "%d-%m-%Y", "%Y-%m-%d"]
            .iter()
            .find_map(|format| NaiveDate::parse_from_str(word, format).ok())
    }));
    let date = unique(dates)?;
    let times: BTreeSet<NaiveTime> = words
        .iter()
        .enumerate()
        .filter_map(|(index, word)| clock_time(word, words.get(index + 1).copied()))
        .collect();
    let time = unique(times)?;
    zone.from_local_datetime(&date.and_time(time))
        .single()
        .map(|date| date.fixed_offset())
}

/// A time of day written as 17:00, 17.00, 5pm, 5:30pm or "5 pm". An am or pm marker, attached
/// or as the next word, is checked before the 24-hour forms, so "3:30 pm" is 15:30.
fn clock_time(word: &str, next: Option<&str>) -> Option<NaiveTime> {
    let lower = word.to_lowercase();
    let following = next.map(str::to_lowercase);
    let (clock, meridiem) = if let Some(clock) = lower.strip_suffix("pm") {
        (clock.to_owned(), Some("pm"))
    } else if let Some(clock) = lower.strip_suffix("am") {
        (clock.to_owned(), Some("am"))
    } else {
        match following.as_deref() {
            Some("pm") => (lower.clone(), Some("pm")),
            Some("am") => (lower.clone(), Some("am")),
            _ => (lower.clone(), None),
        }
    };
    let Some(meridiem) = meridiem else {
        return ["%H:%M", "%H.%M"]
            .iter()
            .find_map(|format| NaiveTime::parse_from_str(&clock, format).ok());
    };
    let (hour, minute) = match clock.split_once([':', '.']) {
        Some((hour, minute)) => (hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?),
        None => (clock.parse::<u32>().ok()?, 0),
    };
    if !(1..=12).contains(&hour) {
        return None;
    }
    let hour = match meridiem {
        "pm" if hour != 12 => hour + 12,
        "am" if hour == 12 => 0,
        _ => hour,
    };
    NaiveTime::from_hms_opt(hour, minute, 0)
}

/// The timezone of the country a portal's domain belongs to, for deadlines written in local
/// time without an offset. Covers European country domains that use a single timezone.
pub fn country_timezone(host: &str) -> Option<Tz> {
    let country = host.rsplit('.').next()?.to_ascii_lowercase();
    let zone = match country.as_str() {
        "uk" => "Europe/London",
        "ie" => "Europe/Dublin",
        "pt" => "Europe/Lisbon",
        "is" => "Atlantic/Reykjavik",
        "fr" => "Europe/Paris",
        "de" => "Europe/Berlin",
        "nl" => "Europe/Amsterdam",
        "be" => "Europe/Brussels",
        "lu" => "Europe/Luxembourg",
        "es" => "Europe/Madrid",
        "it" => "Europe/Rome",
        "at" => "Europe/Vienna",
        "ch" => "Europe/Zurich",
        "dk" => "Europe/Copenhagen",
        "se" => "Europe/Stockholm",
        "no" => "Europe/Oslo",
        "pl" => "Europe/Warsaw",
        "cz" => "Europe/Prague",
        "sk" => "Europe/Bratislava",
        "hu" => "Europe/Budapest",
        "si" => "Europe/Ljubljana",
        "hr" => "Europe/Zagreb",
        "mt" => "Europe/Malta",
        "fi" => "Europe/Helsinki",
        "ee" => "Europe/Tallinn",
        "lv" => "Europe/Riga",
        "lt" => "Europe/Vilnius",
        "ro" => "Europe/Bucharest",
        "bg" => "Europe/Sofia",
        "gr" => "Europe/Athens",
        "cy" => "Asia/Nicosia",
        _ => return None,
    };
    zone.parse().ok()
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
            buyer(html.root_element(), &["from".into()]).as_deref(),
            Some("Legal Aid Agency")
        );
        assert_eq!(buyer(html.root_element(), &[]), None);
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

    fn deadline_in(source: &str) -> Option<String> {
        let html = scraper::Html::parse_fragment(source);
        submission_deadline(html.root_element(), chrono_tz::Europe::London)
            .map(|date| date.to_rfc3339())
    }

    #[test]
    fn reads_deadlines_in_general_wording_and_skips_other_dates() {
        for (source, expected) in [
            // GOV.UK "Re-opening: 2024 Standard Civil Contract".
            (
                "<p>Tenders must be submitted using the LAA's eTendering system and must be submitted by 11:59 on 31 December 2025. Applicants may submit any questions they have via eTendering.</p>",
                Some("2025-12-31T11:59:00+00:00"),
            ),
            // A questions deadline and the submission deadline in one list.
            (
                "<ul><li>Deadline for questions: 30 July 2025 (17:00)</li><li>Tender submission deadline: 28 August 2025 (17:00)</li></ul>",
                Some("2025-08-28T17:00:00+01:00"),
            ),
            // Two wordings of the same deadline, one with "5pm".
            (
                "<p>The deadline for submitting a tender is 17:00 on 10 March 2025.</p><p>The deadline for the submission of tenders is 5pm on 10 March 2025.</p>",
                Some("2025-03-10T17:00:00+00:00"),
            ),
            (
                "<p>Angebotsfrist: 21.12.2026 12.00 Uhr</p>",
                Some("2026-12-21T12:00:00+00:00"),
            ),
            (
                "<p>Offers no later than 3:30 pm on 2 February 2027.</p>",
                Some("2027-02-02T15:30:00+00:00"),
            ),
            // GOV.UK Crime Contract: a past deadline for one stage only.
            (
                "<p>The deadline for the submission of tenders under Stage 1 of this procurement process is 23:59 on 22 October 2024. Stage 3 will close on, or around, 30 September 2034.</p>",
                None,
            ),
            // An expression of interest is not a tender deadline.
            (
                "<p>Please contact us no later than 5pm on 13 November 2026 to submit an expression of interest.</p>",
                None,
            ),
        ] {
            assert_eq!(deadline_in(source).as_deref(), expected, "{source}");
        }
    }

    #[test]
    fn country_domains_give_their_timezone() {
        assert_eq!(
            country_timezone("www.gov.uk"),
            Some(chrono_tz::Europe::London)
        );
        assert_eq!(country_timezone("dtvp.de"), Some(chrono_tz::Europe::Berlin));
        assert_eq!(country_timezone("ted.europa.eu"), None);
        assert_eq!(country_timezone("example.com"), None);
    }
}
