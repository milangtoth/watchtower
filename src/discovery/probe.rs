//! Probing: cheap requests that find the ways a portal's tender list can be read, before any
//! model call. The candidates feed the ranker and the model's connection decision.
//!
//! Detectors run in order within `REQUEST_BUDGET` requests:
//! 1. the start page (HTML alternates and machine links, or a JSON, OCDS, feed or sitemap body).
//!    When it offers no machine links, as with a JavaScript app shell, the site's home page is
//!    read too (following one `<meta http-equiv="refresh">`). Pages of the GOV.UK publishing
//!    platform, recognised by their `govuk:` meta tags, offer their Content API JSON twin,
//! 2. sitemaps from `robots.txt`, `<link rel="sitemap">` or `/sitemap.xml`,
//! 3. discovered feed or JSON URLs and endpoints named by linked API documentation,
//! 4. for a thin app shell, verify public GET listing URLs advertised in same-origin bundles,
//! 5. links to TED notices as evidence, and the usual HTML fallback when discovery succeeds.
//!
//! Detection is generic: there are no per-portal rules, only platform conventions, so nothing
//! depends on which portals are in the test set. Every request goes through the portal's rate-limited client. A login or
//! CAPTCHA on the start page ends probing with `ReadFailure::Blocked`. A plain HTTP 403 on the
//! start page does not: the site's sitemaps may still be readable, so they are still tried.

use crate::{
    domain::{ConnectionCandidate, ConnectionMethod, ReadFailure},
    fetch::{PortalHttp, Sample},
    formats,
};
use reqwest::Url;
use scraper::{Html, Selector};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};

mod bundles;

/// The most requests one probe makes, redirects excluded (the client limits those).
pub(crate) const REQUEST_BUDGET: usize = 12;
/// `robots.txt` plus up to three sitemap files.
const SITEMAP_REQUESTS: usize = 4;
/// Discovered feed or JSON URLs sampled, one request each.
const MAX_SAMPLES: usize = 6;
/// Same-site links whose path or text suggests machine access, before sampling.
const MAX_MACHINE_LINKS: usize = 4;
/// The home page and one meta refresh, read when the start page offers no machine links.
const HOME_REQUESTS: usize = 2;
/// Endpoints named by an API documentation page that are sampled.
const MAX_DOC_ENDPOINTS: usize = 2;
/// Table headers and labels longer than this are content, not field names.
const LABEL_LENGTH: usize = 60;
/// JSON items and feed entries inspected for field names.
const FIELD_SAMPLE: usize = 20;

/// The shared field vocabulary for `fields_seen`, in reporting order. The ranker's features
/// use these exact names.
pub(crate) const FIELDS: [&str; 7] = [
    "title",
    "deadline",
    "buyer",
    "publicationDate",
    "documents",
    "cpv",
    "id",
];

/// Words in a path or link text that suggest a machine-readable listing.
const MACHINE_WORDS: [&str; 11] = [
    "api", "json", "ocds", "rss", "atom", "feed", "feeds", "export", "exports", "opendata", "xml",
];

/// Path fragments that suggest a tender or notice detail page, in several languages.
const NOTICE_WORDS: [&str; 17] = [
    "tender",
    "procurem",
    "contract",
    "notice",
    "opportunit",
    "aanbested",
    "aankondiging",
    "appel",
    "marche",
    "vergabe",
    "ausschreibung",
    "licitaci",
    "zamowien",
    "zakazk",
    "avis",
    "cft",
    "bekanntmachung",
];

/// Finds candidate connection methods for a portal. Every request goes through `http`, which
/// enforces one request per second per portal. A login or CAPTCHA on the start page is reported
/// as `ReadFailure::Blocked`; so is a refused start page when no other route was found.
pub async fn probe(
    start: &Url,
    http: &PortalHttp,
) -> Result<Vec<ConnectionCandidate>, ReadFailure> {
    probe_with(start, http).await
}

/// Where probe requests go. `PortalHttp` in production; canned responses in tests.
pub(crate) trait Source {
    async fn sample(&self, url: Url) -> Result<Sample, ReadFailure>;
    async fn scoped_sample(
        &self,
        url: Url,
        limit: usize,
        prefix: bool,
    ) -> Result<Sample, ReadFailure>;
}

impl Source for PortalHttp {
    async fn scoped_sample(
        &self,
        url: Url,
        limit: usize,
        prefix: bool,
    ) -> Result<Sample, ReadFailure> {
        PortalHttp::scoped_sample(self, url, limit, prefix).await
    }
    async fn sample(&self, url: Url) -> Result<Sample, ReadFailure> {
        PortalHttp::sample(self, url).await
    }
}

pub(crate) async fn probe_with<S: Source>(
    start: &Url,
    source: &S,
) -> Result<Vec<ConnectionCandidate>, ReadFailure> {
    Probe {
        start: start.clone(),
        source,
        used: 0,
        candidates: Vec::new(),
    }
    .run()
    .await
}

/// A URL worth sampling and how it was found.
#[derive(Debug, Clone)]
struct Lead {
    url: Url,
    found_via: &'static str,
    /// The method implied by how the link was advertised, used when the sample fails.
    hint: Option<ConnectionMethod>,
}

struct Probe<'a, S> {
    start: Url,
    source: &'a S,
    used: usize,
    candidates: Vec<ConnectionCandidate>,
}

impl<S: Source> Probe<'_, S> {
    async fn run(mut self) -> Result<Vec<ConnectionCandidate>, ReadFailure> {
        let mut fallback = ConnectionCandidate {
            method: ConnectionMethod::Html,
            entry_url: self.start.to_string(),
            found_via: "start page".into(),
            http_status: 0,
            content_type: None,
            sample_items: 0,
            fields_seen: Vec::new(),
            has_pagination: false,
            score: 0.0,
        };
        let mut leads = Vec::new();
        let mut sitemap_roots = Vec::new();
        let mut ted = None;
        let mut refused = None;
        let mut shell = None;

        // 1. Start page.
        match self.fetch(self.start.clone()).await {
            Some(Ok(sample)) => {
                fallback.http_status = sample.status;
                fallback.content_type = sample.content_type.clone();
                match parse_body(&sample.body, sample.content_type.as_deref()) {
                    Parsed::Html => {
                        let page = inspect_html(&sample.url, &sample.body);
                        fallback.entry_url = sample.url.to_string();
                        fallback.sample_items = page.notice_links;
                        fallback.fields_seen = page.fields.clone();
                        fallback.has_pagination = page.has_next;
                        shell = page.shell;
                        if page.govuk
                            && let Some(url) = govuk_content_api(&sample.url)
                        {
                            leads.push(Lead {
                                url,
                                found_via: "GOV.UK content API",
                                hint: Some(ConnectionMethod::Json),
                            });
                        }
                        leads.extend(page.leads);
                        sitemap_roots.extend(page.sitemaps);
                        ted = page.ted;
                        if leads.is_empty() {
                            let home = self.home_page(&sample.url).await;
                            leads.extend(home.leads);
                            sitemap_roots.extend(home.sitemaps);
                            if ted.is_none() {
                                ted = home.ted;
                            }
                        }
                    }
                    parsed => self.push_parsed(&sample, parsed, "start page", None),
                }
            }
            Some(Err(failure @ ReadFailure::Blocked(_))) => return Err(failure),
            Some(Err(failure)) if failure.is_temporary() => return Err(failure),
            Some(Err(failure)) => {
                fallback.http_status = http_status(&failure).unwrap_or(0);
                refused = Some(failure);
            }
            None => unreachable!("the start page is the first request"),
        }

        if self.start.host_str().is_some_and(is_ted_host) {
            ted.get_or_insert((self.start.to_string(), 0));
        }

        // 2. Sitemaps.
        self.sitemaps(sitemap_roots).await;

        // 3. One sample of each discovered feed or JSON URL. An API or data link that leads to
        //    an HTML page is documentation: the endpoints it names are sampled too (once).
        let mut sampled = HashSet::new();
        let mut documentation_read = false;
        for lead in leads {
            if sampled.len() >= MAX_SAMPLES || self.used >= REQUEST_BUDGET {
                break;
            }
            if sampled.insert(lead.url.clone()) {
                let api_link = matches!(
                    lead.hint,
                    Some(ConnectionMethod::Json | ConnectionMethod::Ocds)
                );
                if let Some(page) = self.sample_lead(lead).await
                    && api_link
                    && !documentation_read
                {
                    documentation_read = true;
                    self.api_documentation(&page).await;
                }
            }
        }

        // Only sample JavaScript when the start page is an application shell.
        let mut post_search = false;
        if let Some(shell) = &shell {
            match self.bundle_listings(shell, &sampled).await {
                Ok(post) => post_search = post,
                Err(err) if !self.candidates.iter().any(ConnectionCandidate::is_usable) => {
                    return Err(err);
                }
                Err(_) => {} // Stop sampling and keep the usable route already found.
            }
        }

        // 4. TED.
        if let Some((entry_url, notices)) = ted {
            self.push(ConnectionCandidate {
                method: ConnectionMethod::Ted,
                entry_url,
                found_via: if notices == 0 {
                    "start page".into()
                } else {
                    "link to ted.europa.eu".into()
                },
                http_status: 0,
                content_type: None,
                sample_items: notices,
                fields_seen: Vec::new(),
                has_pagination: false,
                score: 0.0,
            });
        }

        // 5. HTML fallback, unless the start page was refused and nothing else was found.
        if let Some(failure) = refused
            && self.candidates.is_empty()
        {
            return Err(failure);
        }
        if shell.is_some() && !self.candidates.iter().any(ConnectionCandidate::is_usable) {
            let reason = if post_search {
                "The JavaScript app shell exposes a search that cannot be read with GET. POST search is unsupported"
            } else {
                "The JavaScript app shell exposed no verified public GET listing. Searches requiring POST are unsupported"
            };
            return Err(ReadFailure::Incomplete(reason.into()));
        }
        self.push(fallback);
        Ok(self.candidates)
    }

    async fn bundle_listings(
        &mut self,
        shell: &bundles::Shell,
        sampled: &HashSet<Url>,
    ) -> Result<bool, ReadFailure> {
        let mut routes = bundles::Routes::default();
        for url in &shell.scripts {
            if self.used + bundles::MAX_ENDPOINTS >= REQUEST_BUDGET {
                break;
            }
            self.used += 1;
            match self
                .source
                .scoped_sample(url.clone(), bundles::SAMPLE_BYTES, true)
                .await
            {
                Ok(bundle) => routes.inspect(&bundle.body, &shell.base, &shell.page),
                Err(err @ ReadFailure::Throttled { .. }) => return Err(err),
                Err(err) if http_status(&err) == Some(429) => return Err(err),
                Err(_) => continue,
            }
        }
        let mut post_search = routes.post_search;
        for url in routes
            .best()
            .filter(|url| !sampled.contains(url))
            .take(bundles::MAX_ENDPOINTS)
        {
            if self.used >= REQUEST_BUDGET {
                break;
            }
            self.used += 1;
            let sample = match self
                .source
                .scoped_sample(url, bundles::SAMPLE_BYTES, false)
                .await
            {
                Ok(sample) => sample,
                Err(err @ ReadFailure::Throttled { .. }) => return Err(err),
                Err(err) if http_status(&err) == Some(429) => return Err(err),
                Err(err) => {
                    post_search |= http_status(&err) == Some(405);
                    continue;
                }
            };
            let Ok(value) = serde_json::from_str::<Value>(&sample.body) else {
                continue;
            };
            if value.get("errors").is_some() && value.get("data").is_none_or(Value::is_null) {
                continue;
            }
            if let Some(listing) = json_listing(&value)
                && listing.items > 0
                && !listing.fields.is_empty()
            {
                self.push_parsed(
                    &sample,
                    Parsed::Listing(listing),
                    "app-shell bundle GET",
                    None,
                );
            }
        }
        Ok(post_search)
    }

    /// Reads the site's home page when the start page offers no machine links, as with a
    /// JavaScript app shell whose links only exist after scripts run. Follows one same-site
    /// `<meta http-equiv="refresh">`. Links found there are reported as home page links.
    async fn home_page(&mut self, start: &Url) -> HtmlPage {
        let mut found = HtmlPage::default();
        let Ok(mut url) = start.join("/") else {
            return found;
        };
        for _ in 0..HOME_REQUESTS {
            if url.path() == start.path() && url.query() == start.query() {
                break;
            }
            let Some(Ok(sample)) = self.fetch(url.clone()).await else {
                break;
            };
            if parse_body(&sample.body, sample.content_type.as_deref()) != Parsed::Html {
                break;
            }
            let page = inspect_html(&sample.url, &sample.body);
            match page.refresh.clone() {
                Some(next) if page.leads.is_empty() && related_host(&next, start) => url = next,
                _ => {
                    found = page;
                    break;
                }
            }
        }
        for lead in &mut found.leads {
            if lead.found_via == "start page link" {
                lead.found_via = "home page link";
            }
        }
        found
    }

    async fn fetch(&mut self, url: Url) -> Option<Result<Sample, ReadFailure>> {
        if self.used >= REQUEST_BUDGET {
            return None;
        }
        self.used += 1;
        Some(self.source.sample(url).await)
    }

    fn push(&mut self, candidate: ConnectionCandidate) {
        let duplicate = self.candidates.iter().any(|existing| {
            existing.method == candidate.method && existing.entry_url == candidate.entry_url
        });
        if !duplicate {
            self.candidates.push(candidate);
        }
    }

    fn push_parsed(
        &mut self,
        sample: &Sample,
        parsed: Parsed,
        found_via: &str,
        entry: Option<&Url>,
    ) {
        let Some(listing) = parsed.listing() else {
            return;
        };
        self.push(ConnectionCandidate {
            method: listing.method,
            entry_url: entry.unwrap_or(&sample.url).to_string(),
            found_via: found_via.into(),
            http_status: sample.status,
            content_type: sample.content_type.clone(),
            sample_items: listing.items,
            fields_seen: listing.fields,
            has_pagination: listing.has_pagination,
            score: 0.0,
        });
    }

    /// Samples one lead and adds its candidate. Returns the sample when it was an HTML page.
    async fn sample_lead(&mut self, lead: Lead) -> Option<Sample> {
        let result = self.fetch(lead.url.clone()).await?;
        match result {
            Ok(sample) => {
                let parsed = parse_body(&sample.body, sample.content_type.as_deref());
                if parsed == Parsed::Html {
                    return Some(sample);
                }
                self.push_parsed(&sample, parsed, lead.found_via, None);
            }
            // A refused route is still evidence for the decision; a blocked or flaky one is not.
            Err(failure) => {
                if let (Some(status), Some(method)) = (http_status(&failure), lead.hint) {
                    self.push(ConnectionCandidate {
                        method,
                        entry_url: lead.url.to_string(),
                        found_via: lead.found_via.into(),
                        http_status: status,
                        content_type: None,
                        sample_items: 0,
                        fields_seen: Vec::new(),
                        has_pagination: false,
                        score: 0.0,
                    });
                }
            }
        }
        None
    }

    /// Follows API documentation reached through an API or data link: samples the same-site
    /// endpoints the page names. When it names none, reads one documentation subpage first,
    /// preferring one about OCDS releases or notices.
    async fn api_documentation(&mut self, page: &Sample) {
        let mut endpoints = api_endpoints(&page.url, &page.body);
        if endpoints.is_empty()
            && let Some(subpage) = documentation_subpage(&page.url, &page.body)
            && let Some(Ok(sample)) = self.fetch(subpage).await
            && parse_body(&sample.body, sample.content_type.as_deref()) == Parsed::Html
        {
            endpoints = api_endpoints(&sample.url, &sample.body);
        }
        for url in endpoints.into_iter().take(MAX_DOC_ENDPOINTS) {
            let hint = Some(json_hint(&url));
            self.sample_lead(Lead {
                url,
                found_via: "API documentation",
                hint,
            })
            .await;
        }
    }

    /// Reads `robots.txt` and the sitemaps it names (or `/sitemap.xml`), following one child of
    /// a sitemap index. Adds a candidate for the first sitemap that lists tender detail pages.
    async fn sitemaps(&mut self, mut roots: Vec<(Url, &'static str)>) {
        let mut requests = 0;
        if let Ok(robots) = self.start.join("/robots.txt")
            && let Some(Ok(sample)) = self.fetch(robots).await
        {
            for url in robots_sitemaps(&sample.body, &sample.url) {
                if related_host(&url, &self.start) {
                    roots.push((url, "robots.txt"));
                }
            }
        }
        requests += 1;
        if roots.is_empty()
            && let Ok(url) = self.start.join("/sitemap.xml")
        {
            roots.push((url, "/sitemap.xml"));
        }
        let mut seen = HashSet::new();
        roots.retain(|(url, _)| seen.insert(url.clone()));
        roots.sort_by_key(|(url, _)| !notice_path(url));

        for (root, found_via) in roots {
            if requests >= SITEMAP_REQUESTS {
                return;
            }
            requests += 1;
            let Some(Ok(sample)) = self.fetch(root.clone()).await else {
                continue;
            };
            let Some(map) = read_sitemap(&sample.body) else {
                continue;
            };
            let (mut tender_pages, mut children) = (count_notice_urls(&map.urls), 0);
            if !map.sitemaps.is_empty() {
                children = map.sitemaps.len();
                let mut child_urls: Vec<Url> = map
                    .sitemaps
                    .iter()
                    .filter_map(|loc| sample.url.join(loc).ok())
                    .filter(|url| related_host(url, &self.start))
                    .collect();
                child_urls.sort_by_key(|url| !notice_path(url));
                if let Some(child) = child_urls.into_iter().next()
                    && requests < SITEMAP_REQUESTS
                {
                    requests += 1;
                    if let Some(Ok(child)) = self.fetch(child).await
                        && let Some(child_map) = read_sitemap(&child.body)
                    {
                        tender_pages += count_notice_urls(&child_map.urls);
                    }
                }
            }
            if tender_pages > 0 {
                self.push(ConnectionCandidate {
                    method: ConnectionMethod::Sitemap,
                    entry_url: sample.url.to_string(),
                    found_via: found_via.into(),
                    http_status: sample.status,
                    content_type: sample.content_type.clone(),
                    sample_items: tender_pages,
                    fields_seen: Vec::new(),
                    has_pagination: children > 1,
                    score: 0.0,
                });
                return;
            }
        }
    }
}

/// What a fetched body turned out to be.
#[derive(Debug, PartialEq)]
pub(crate) enum Parsed {
    Listing(Listing),
    Html,
    Unknown,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Listing {
    pub method: ConnectionMethod,
    pub items: usize,
    pub fields: Vec<String>,
    pub has_pagination: bool,
}

impl Parsed {
    fn listing(self) -> Option<Listing> {
        match self {
            Self::Listing(listing) => Some(listing),
            _ => None,
        }
    }
}

/// Classifies a body as an OCDS package, a JSON listing, a feed, a sitemap or an HTML page.
pub(crate) fn parse_body(body: &str, content_type: Option<&str>) -> Parsed {
    let text = body.trim_start_matches('\u{feff}').trim_start();
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
    if text.starts_with(['{', '[']) || content_type.contains("json") {
        return serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|value| json_listing(&value))
            .map_or(Parsed::Unknown, Parsed::Listing);
    }
    let head: String = text
        .chars()
        .take(2048)
        .collect::<String>()
        .to_ascii_lowercase();
    if head.contains("<html") || head.starts_with("<!doctype html") {
        return Parsed::Html;
    }
    // Never expand entities or DTDs from a portal: a document type in XML is refused outright.
    if head.contains("<!doctype") || head.contains("<!entity") {
        return Parsed::Unknown;
    }
    if let Some(listing) = feed_listing(text) {
        return Parsed::Listing(listing);
    }
    if let Some(map) = read_sitemap(text) {
        return Parsed::Listing(Listing {
            method: ConnectionMethod::Sitemap,
            items: count_notice_urls(&map.urls),
            fields: Vec::new(),
            has_pagination: map.sitemaps.len() > 1,
        });
    }
    if content_type.contains("html") || head.contains("<body") {
        Parsed::Html
    } else {
        Parsed::Unknown
    }
}

fn read_sitemap(body: &str) -> Option<formats::Sitemap> {
    let head: String = body
        .chars()
        .take(2048)
        .collect::<String>()
        .to_ascii_lowercase();
    if head.contains("<!doctype") || head.contains("<!entity") {
        return None;
    }
    formats::sitemap(body).ok()
}

fn json_listing(value: &Value) -> Option<Listing> {
    if is_ocds(value) {
        let page = formats::ocds(value).ok()?;
        let mut names = BTreeSet::new();
        for tender in page.tenders.iter().take(FIELD_SAMPLE) {
            for key in tender.fields.keys() {
                names.extend(ocds_field(key));
            }
            if !tender.documents.is_empty() {
                names.insert("documents");
            }
        }
        return Some(Listing {
            method: ConnectionMethod::Ocds,
            items: page.tenders.len(),
            fields: ordered(names),
            has_pagination: page.next.is_some(),
        });
    }
    let items = items_array(value, 0)?;
    let mut names = BTreeSet::new();
    for item in items.iter().take(FIELD_SAMPLE) {
        collect_json_fields(item, 0, &mut names);
    }
    Some(Listing {
        method: ConnectionMethod::Json,
        items: items.len(),
        fields: ordered(names),
        has_pagination: json_pagination(value, items.len()),
    })
}

/// A release or record package whose entries carry an `ocid`.
fn is_ocds(value: &Value) -> bool {
    ["releases", "records"].iter().any(|key| {
        value[key]
            .as_array()
            .and_then(|entries| entries.first())
            .is_some_and(|entry| entry.get("ocid").is_some())
    })
}

/// The largest array of objects within three levels, which is most likely the item list.
fn items_array(value: &Value, depth: usize) -> Option<&Vec<Value>> {
    let own = value
        .as_array()
        .filter(|items| !items.is_empty() && items.iter().all(Value::is_object));
    let nested = match value {
        Value::Object(map) if depth < 3 => map
            .values()
            .filter_map(|child| items_array(child, depth + 1))
            .max_by_key(|items| items.len()),
        _ => None,
    };
    match (own, nested) {
        (Some(own), Some(nested)) if nested.len() > own.len() => Some(nested),
        (Some(own), _) => Some(own),
        (None, nested) => nested,
    }
}

fn collect_json_fields(value: &Value, depth: usize, names: &mut BTreeSet<&'static str>) {
    let Value::Object(map) = value else {
        return;
    };
    for (key, child) in map {
        let present = match child {
            Value::Null => false,
            Value::String(text) => !text.trim().is_empty(),
            Value::Array(items) => !items.is_empty(),
            _ => true,
        };
        if !present {
            continue;
        }
        names.extend(field_for_key(key));
        if depth < 1 {
            collect_json_fields(child, depth + 1, names);
        }
    }
}

fn json_pagination(value: &Value, items: usize) -> bool {
    let Value::Object(map) = value else {
        return false;
    };
    if value["links"]["next"].is_string() {
        return true;
    }
    map.iter().any(|(key, child)| {
        let key = key.to_ascii_lowercase();
        let next = matches!(
            key.as_str(),
            "next"
                | "nextpage"
                | "next_page"
                | "nextcursor"
                | "cursor"
                | "page"
                | "pagenumber"
                | "totalpages"
                | "total_pages"
                | "pagecount"
                | "offset"
        ) && !child.is_null();
        let more = matches!(
            key.as_str(),
            "total" | "total_count" | "totalcount" | "count"
        ) && child.as_u64().is_some_and(|total| total > items as u64);
        next || more
    })
}

fn ocds_field(key: &str) -> Option<&'static str> {
    match key {
        "title" => Some("title"),
        "submissionDeadline" => Some("deadline"),
        "buyerName" => Some("buyer"),
        "publicationDate" => Some("publicationDate"),
        "cpvCodes" => Some("cpv"),
        "id" => Some("id"),
        _ => None,
    }
}

/// Maps a JSON key or XML element name to the shared field vocabulary.
pub(crate) fn field_for_key(key: &str) -> Option<&'static str> {
    let key: String = key
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| key.contains(word));
    let named_id = key == "id"
        || [
            "notice",
            "content",
            "tender",
            "object",
            "record",
            "release",
            "procedure",
            "contract",
            "publication",
        ]
        .iter()
        .any(|noun| key.strip_suffix("id") == Some(noun));
    if named_id
        || matches!(
            key.as_str(),
            "ocid"
                | "ref"
                | "reference"
                | "identifier"
                | "guid"
                | "noticenumber"
                | "idweb"
                | "publicationnumber"
                | "referencenumber"
                | "kenmerk"
                | "aktenzeichen"
                | "referencia"
        )
        || has(&["expedient", "referentie", "číslozakázky", "numerogara"])
    {
        Some("id")
    } else if has(&[
        "deadline",
        "closing",
        "submission",
        "enddate",
        "datelimite",
        "sluiting",
        "uiterste",
        "tenderperiod",
        "angebotsfrist",
        "einreichung",
        "frist",
        "termini",
        "plazo",
        "fechalimite",
        "datalimit",
        "scadenza",
        "lhůta",
        "termínpro",
        "terminskład",
    ]) {
        Some("deadline")
    } else if has(&[
        "publish",
        "publication",
        "publicatie",
        "parution",
        "pubdate",
        "issued",
        "veröffentlich",
        "veroffentlich",
        "veroeffentlich",
        "publicaci",
        "publicació",
        "pubblica",
        "publikac",
        "zveřejn",
        "uveřejn",
    ]) {
        Some("publicationDate")
    } else if has(&[
        "buyer",
        "authority",
        "acheteur",
        "opdrachtgever",
        "aanbestedende",
        "organization",
        "organisation",
        "contractingbody",
        "purchaser",
        "auftraggeber",
        "vergabestelle",
        "òrgan",
        "organdecontract",
        "organcontract",
        "órganodecontrat",
        "organodecontrat",
        "poderadjudicador",
        "entecontraente",
        "stazioneappaltante",
        "zadavatel",
        "zamawiaj",
    ]) {
        Some("buyer")
    } else if has(&[
        "document",
        "dokument",
        "attachment",
        "enclosure",
        "files",
        "unterlagen",
        "bijlage",
        "příloh",
        "allegat",
        "adjunt",
    ]) {
        Some("documents")
    } else if has(&["cpv", "classification"]) {
        Some("cpv")
    } else if has(&[
        "title",
        "titel",
        "titol",
        "titulo",
        "título",
        "titolo",
        "tytuł",
        "název",
        "nazev",
        "nazwa",
        "objet",
        "objeto",
        "objecte",
        "oggetto",
        "orderobject",
        "subject",
        "onderwerp",
        "intitule",
        "intitulé",
        "betreff",
        "bezeichnung",
        "předmět",
    ]) {
        Some("title")
    } else {
        None
    }
}

fn ordered(names: BTreeSet<&'static str>) -> Vec<String> {
    FIELDS
        .iter()
        .filter(|field| names.contains(*field))
        .map(|field| (*field).to_owned())
        .collect()
}

/// An RSS or Atom feed with its entries' field names.
fn feed_listing(body: &str) -> Option<Listing> {
    let feed = formats::feed(body).ok()?;
    let mut names = BTreeSet::new();
    let mut reader = quick_xml::Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let (mut depth, mut entry_depth, mut entries) = (0_usize, None, 0_usize);
    let mut current: Option<String> = None;
    loop {
        use quick_xml::events::Event;
        let (element, opened) = match reader.read_event() {
            Ok(Event::Start(element)) => (element, true),
            Ok(Event::Empty(element)) => (element, false),
            Ok(Event::Text(text)) => {
                if current.as_deref() == Some("category")
                    && let Ok(text) = text.unescape()
                    && text.chars().filter(char::is_ascii_digit).count() >= 8
                {
                    names.insert("cpv");
                }
                continue;
            }
            Ok(Event::End(_)) => {
                depth = depth.saturating_sub(1);
                current = None;
                if entry_depth == Some(depth) {
                    entry_depth = None;
                }
                continue;
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => continue,
        };
        let name = String::from_utf8_lossy(element.local_name().as_ref()).into_owned();
        let lower = name.to_ascii_lowercase();
        if entry_depth.is_none() && opened && matches!(lower.as_str(), "item" | "entry") {
            entry_depth = Some(depth);
            entries += 1;
        } else if entry_depth.is_some() && entries <= FIELD_SAMPLE {
            match lower.as_str() {
                "link" | "guid" | "id" => {
                    names.insert("id");
                }
                "updated" | "date" => {
                    names.insert("publicationDate");
                }
                "category" if opened => current = Some(lower),
                _ => names.extend(field_for_key(&name)),
            }
        }
        if opened {
            depth += 1;
        }
    }
    Some(Listing {
        method: ConnectionMethod::Feed,
        items: feed.entries.len(),
        fields: ordered(names),
        has_pagination: feed.next.is_some(),
    })
}

/// What the start page's HTML offers.
#[derive(Debug, Default)]
struct HtmlPage {
    shell: Option<bundles::Shell>,
    leads: Vec<Lead>,
    sitemaps: Vec<(Url, &'static str)>,
    /// The first linked TED notice and how many distinct TED notices are linked.
    ted: Option<(String, usize)>,
    notice_links: usize,
    has_next: bool,
    /// Field names from table headers, definition terms and labels, in the shared vocabulary.
    fields: Vec<String>,
    /// A same-site `<meta http-equiv="refresh">` target.
    refresh: Option<Url>,
    /// The page carries GOV.UK publishing platform metadata (`<meta name="govuk:...">`).
    govuk: bool,
}

/// Reads an HTML page without holding the parsed document across an await.
fn inspect_html(base: &Url, body: &str) -> HtmlPage {
    let document = Html::parse_document(body);
    let mut page = HtmlPage::default();
    let selector = |css: &str| Selector::parse(css).expect("static selector");

    for link in document.select(&selector("link[rel][href]")) {
        let rel = link
            .value()
            .attr("rel")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let rels: Vec<&str> = rel.split_ascii_whitespace().collect();
        let Some(url) = link
            .value()
            .attr("href")
            .and_then(|href| resolve(base, href))
        else {
            continue;
        };
        if rels.contains(&"alternate") {
            let kind = link
                .value()
                .attr("type")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let hint = if kind.contains("rss") || kind.contains("atom") {
                Some(ConnectionMethod::Feed)
            } else if kind.contains("json") {
                Some(json_hint(&url))
            } else {
                None
            };
            if hint.is_some() {
                page.leads.push(Lead {
                    url,
                    found_via: "link rel=alternate",
                    hint,
                });
            }
        } else if rels.contains(&"sitemap") && related_host(&url, base) {
            page.sitemaps.push((url, "link rel=sitemap"));
        } else if rels.contains(&"next") {
            page.has_next = true;
        }
    }

    let mut machine = 0;
    let mut notices = HashSet::new();
    let mut ted_notices = Vec::new();
    for anchor in document.select(&selector("a[href]")) {
        let Some(url) = anchor
            .value()
            .attr("href")
            .and_then(|href| resolve(base, href))
        else {
            continue;
        };
        let text = anchor.text().collect::<Vec<_>>().join(" ");
        let rel = anchor
            .value()
            .attr("rel")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if url.host_str().is_some_and(is_ted_host) {
            if ted_link(&url) && !ted_notices.contains(&url) {
                ted_notices.push(url);
            }
            continue;
        }
        if !related_host(&url, base) {
            continue;
        }
        if rel.split_ascii_whitespace().any(|value| value == "next")
            || next_label(&text)
            || page_parameter(&url)
        {
            page.has_next = true;
        }
        if url.path().to_ascii_lowercase().contains("sitemap") {
            page.sitemaps.push((url.clone(), "link rel=sitemap"));
            continue;
        }
        if machine < MAX_MACHINE_LINKS && url != *base && machine_link(&url, &text) {
            machine += 1;
            let hint = machine_hint(&url, &text);
            page.leads.push(Lead {
                url: url.clone(),
                found_via: "start page link",
                hint,
            });
        }
        // Pagination links back to the listing itself are not notices.
        if notice_path(&url) && url.path() != base.path() {
            let mut key = url.clone();
            key.set_fragment(None);
            notices.insert(key);
        }
    }
    page.notice_links = notices.len();
    if let Some(first) = ted_notices.first() {
        page.ted = Some((first.to_string(), ted_notices.len()));
    }

    let mut names = BTreeSet::new();
    for label in document.select(&selector("th, dt, label, caption")) {
        let text = label.text().collect::<Vec<_>>().join(" ");
        let text = text.trim();
        if !text.is_empty() && text.chars().count() <= LABEL_LENGTH {
            names.extend(field_for_key(text));
        }
    }
    // Repeated notice links carry the notice titles; links to files are documents.
    if page.notice_links >= 2 {
        names.insert("title");
    }
    if document.select(&selector("a[href]")).any(|anchor| {
        let href = anchor
            .value()
            .attr("href")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let path = href.split(['?', '#']).next().unwrap_or_default();
        [".pdf", ".zip", ".doc", ".docx", ".xlsx"]
            .iter()
            .any(|extension| path.ends_with(extension))
    }) {
        names.insert("documents");
    }
    page.fields = ordered(names);

    for meta in document.select(&selector("meta[http-equiv][content]")) {
        let equiv = meta.value().attr("http-equiv").unwrap_or_default();
        if equiv.eq_ignore_ascii_case("refresh")
            && let Some(target) = meta.value().attr("content").and_then(refresh_target)
            && let Some(url) = resolve(base, &target)
            && related_host(&url, base)
        {
            page.refresh = Some(url);
        }
    }
    if page.notice_links == 0 {
        page.shell = bundles::scripts(&document, base);
    }
    page.govuk = document.select(&selector("meta[name]")).any(|meta| {
        meta.value()
            .attr("name")
            .is_some_and(|name| name.starts_with("govuk:"))
    });
    page
}

/// The URL in a meta refresh `content` such as `0; url='/cms'`.
fn refresh_target(content: &str) -> Option<String> {
    let lower = content.to_ascii_lowercase();
    let start = lower.find("url=")? + 4;
    let target = content[start..]
        .trim()
        .trim_matches(|c| c == '\'' || c == '"')
        .trim();
    (!target.is_empty()).then(|| target.to_owned())
}

/// The GOV.UK Content API twin of a GOV.UK page: `/api/content` plus the page path.
fn govuk_content_api(page: &Url) -> Option<Url> {
    if page.path().starts_with("/api/") {
        return None;
    }
    let mut url = page.join(&format!("/api/content{}", page.path())).ok()?;
    url.set_query(None);
    Some(url)
}

/// Same-site API endpoints named on a documentation page, in links or in its text (code
/// samples): paths with an `api` segment followed by at least one more. Queries and example
/// IDs are dropped: a path that extends another found path is left out. Endpoints about OCDS
/// releases or notices come first, then shorter paths.
fn api_endpoints(base: &Url, body: &str) -> Vec<Url> {
    let url_char = |c: char| c.is_ascii_alphanumeric() || "-._~/:".contains(c);
    let mut found: Vec<Url> = Vec::new();
    for (at, _) in body.match_indices("/api/") {
        let begin = body[..at]
            .char_indices()
            .rev()
            .find(|&(_, c)| !url_char(c))
            .map_or(0, |(index, c)| index + c.len_utf8());
        let end = at
            + body[at..]
                .char_indices()
                .find(|&(_, c)| !(url_char(c) && c != ':'))
                .map_or(body.len() - at, |(index, _)| index);
        let text = &body[begin..end];
        let text = match text.find("http") {
            Some(scheme) => &text[scheme..],
            None => &text[text.find('/').unwrap_or(0)..],
        };
        if !text.starts_with('/') && !text.starts_with("http") {
            continue;
        }
        let Some(mut url) = resolve(base, text.trim_end_matches(['/', '.'])) else {
            continue;
        };
        url.set_query(None);
        let segments: Vec<String> = url
            .path_segments()
            .map(|parts| {
                parts
                    .filter(|s| !s.is_empty())
                    .map(str::to_lowercase)
                    .collect()
            })
            .unwrap_or_default();
        let Some(api) = segments.iter().position(|segment| segment == "api") else {
            continue;
        };
        let rest = &segments[api + 1..];
        let documentation = rest.iter().any(|s| {
            ["doc", "swagger", "help"]
                .iter()
                .any(|word| s.contains(word))
        });
        if rest.is_empty() || documentation || !related_host(&url, base) {
            continue;
        }
        if !found.contains(&url) {
            found.push(url);
        }
    }
    let extends = |url: &Url, other: &Url| {
        url != other
            && url.host_str() == other.host_str()
            && url.path().starts_with(&format!("{}/", other.path()))
    };
    let mut endpoints: Vec<Url> = found
        .iter()
        .filter(|url| !found.iter().any(|other| extends(url, other)))
        .cloned()
        .collect();
    endpoints.sort_by_key(|url| (!about_notices(url), url.path().len()));
    endpoints
}

/// One same-site API documentation subpage, preferring OCDS releases, then OCDS, then notices.
fn documentation_subpage(base: &Url, body: &str) -> Option<Url> {
    let document = Html::parse_document(body);
    let selector = Selector::parse("a[href]").expect("static selector");
    let mut best: Option<(u8, Url)> = None;
    for anchor in document.select(&selector) {
        let Some(url) = anchor
            .value()
            .attr("href")
            .and_then(|href| resolve(base, href))
        else {
            continue;
        };
        let path = url.path().to_ascii_lowercase();
        if !path.contains("api") || url.path() == base.path() || !related_host(&url, base) {
            continue;
        }
        let rank = if path.contains("ocds") && path.contains("release") {
            0
        } else if path.contains("ocds") {
            1
        } else if notice_path(&url) {
            2
        } else {
            continue;
        };
        if best.as_ref().is_none_or(|(current, _)| rank < *current) {
            best = Some((rank, url));
        }
    }
    best.map(|(_, url)| url)
}

/// An endpoint about OCDS releases or tender notices, judged by its path.
fn about_notices(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    path.contains("ocds") || path.contains("release") || notice_path(url)
}

fn resolve(base: &Url, href: &str) -> Option<Url> {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.starts_with("javascript:") {
        return None;
    }
    let mut url = base.join(href).ok()?;
    url.set_fragment(None);
    crate::fetch::public_http(&url).then_some(url)
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn machine_link(url: &Url, text: &str) -> bool {
    let mut tokens = words(url.path());
    tokens.extend(words(text));
    let open_data = tokens.windows(2).any(|pair| pair == ["open", "data"]);
    open_data
        || tokens
            .iter()
            .any(|token| MACHINE_WORDS.contains(&token.as_str()))
}

fn machine_hint(url: &Url, text: &str) -> Option<ConnectionMethod> {
    let mut tokens = words(url.path());
    tokens.extend(words(text));
    let has = |word: &str| tokens.iter().any(|token| token == word);
    if has("ocds") {
        Some(ConnectionMethod::Ocds)
    } else if has("rss") || has("atom") || has("feed") || has("feeds") {
        Some(ConnectionMethod::Feed)
    } else if has("json") || has("api") {
        Some(ConnectionMethod::Json)
    } else {
        None
    }
}

fn json_hint(url: &Url) -> ConnectionMethod {
    if words(url.path()).iter().any(|word| word == "ocds") {
        ConnectionMethod::Ocds
    } else {
        ConnectionMethod::Json
    }
}

fn next_label(text: &str) -> bool {
    let text = text.trim().to_lowercase();
    text.chars().count() <= 20
        && [
            "next",
            "volgende",
            "suivant",
            "siguiente",
            "weiter",
            "następna",
            "další",
            "›",
            "»",
        ]
        .iter()
        .any(|label| text.starts_with(label))
}

fn page_parameter(url: &Url) -> bool {
    url.query_pairs().any(|(key, value)| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "page" | "p" | "pagenumber" | "page_number" | "pageindex" | "offset" | "start"
        ) && !value.is_empty()
            && value.chars().all(|c| c.is_ascii_digit())
    })
}

fn notice_path(url: &Url) -> bool {
    let path = format!("{}?{}", url.path(), url.query().unwrap_or_default()).to_lowercase();
    NOTICE_WORDS.iter().any(|word| path.contains(word))
}

fn count_notice_urls(urls: &[String]) -> usize {
    urls.iter()
        .filter_map(|loc| Url::parse(loc).ok())
        .filter(notice_path)
        .count()
}

fn robots_sitemaps(body: &str, base: &Url) -> Vec<Url> {
    body.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case("sitemap")
                .then(|| value.split('#').next().unwrap_or_default().trim())
        })
        .filter_map(|value| resolve(base, value))
        .collect()
}

fn is_ted_host(host: &str) -> bool {
    host == "ted.europa.eu" || host.ends_with(".ted.europa.eu")
}

fn ted_link(url: &Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    path.contains("/notice") || path.contains("/detail") || path.contains("/udl")
}

/// The same host, or one host a subdomain of the other (`www.` and `api.` style variants).
fn related_host(url: &Url, base: &Url) -> bool {
    let (Some(host), Some(base)) = (url.host_str(), base.host_str()) else {
        return false;
    };
    let strip = |host: &str| host.trim_start_matches("www.").to_ascii_lowercase();
    let (host, base) = (strip(host), strip(base));
    host == base || host.ends_with(&format!(".{base}")) || base.ends_with(&format!(".{host}"))
}

/// The HTTP status in a failure such as "Portal returned HTTP 403".
fn http_status(failure: &ReadFailure) -> Option<u16> {
    let reason = failure.to_string();
    let (_, rest) = reason.split_once("HTTP ")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse()
        .ok()
        .filter(|status| (100..600).contains(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::HashMap, sync::Mutex};

    /// Canned responses keyed by URL. Unknown URLs answer HTTP 404, like a real portal.
    #[derive(Default)]
    struct Canned {
        pages: HashMap<String, Result<(&'static str, String), ReadFailure>>,
        requests: Mutex<Vec<String>>,
    }

    impl Canned {
        fn page(mut self, url: &str, content_type: &'static str, body: impl Into<String>) -> Self {
            self.pages
                .insert(url.into(), Ok((content_type, body.into())));
            self
        }

        fn failure(mut self, url: &str, failure: ReadFailure) -> Self {
            self.pages.insert(url.into(), Err(failure));
            self
        }

        fn requested(&self) -> Vec<String> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Source for Canned {
        async fn scoped_sample(&self, url: Url, _: usize, _: bool) -> Result<Sample, ReadFailure> {
            self.sample(url).await
        }
        async fn sample(&self, url: Url) -> Result<Sample, ReadFailure> {
            self.requests.lock().unwrap().push(url.to_string());
            match self.pages.get(url.as_str()) {
                Some(Ok((content_type, body))) => Ok(Sample {
                    url,
                    status: 200,
                    content_type: Some((*content_type).into()),
                    body: body.clone(),
                }),
                Some(Err(failure)) => Err(failure.clone()),
                None => Err(ReadFailure::Incomplete("Portal returned HTTP 404".into())),
            }
        }
    }

    fn url(value: &str) -> Url {
        Url::parse(value).unwrap()
    }

    fn methods(candidates: &[ConnectionCandidate]) -> Vec<ConnectionMethod> {
        candidates
            .iter()
            .map(|candidate| candidate.method)
            .collect()
    }

    fn listing(body: &str, content_type: &str) -> Listing {
        match parse_body(body, Some(content_type)) {
            Parsed::Listing(listing) => listing,
            other => panic!("expected a listing, got {other:?}"),
        }
    }

    const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel><title>Tenders</title>
  <item><title>Road works</title><link>https://portal.example/notice/1</link>
    <pubDate>Fri, 20 Nov 2026 09:00:00 GMT</pubDate><category>45233140</category></item>
  <item><title>School meals</title><link>https://portal.example/notice/2</link>
    <guid>n-2</guid><enclosure url="https://portal.example/docs/2.pdf" type="application/pdf"/></item>
</channel></rss>"#;

    const ATOM: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>Notices</title>
  <link rel="next" href="https://portal.example/feed?page=2"/>
  <entry><title>Bridge repair</title><id>urn:notice:1</id>
    <link href="https://portal.example/notice/1"/><updated>2026-11-20T09:00:00Z</updated></entry>
  <entry><title>IT services</title><id>urn:notice:2</id>
    <link href="https://portal.example/notice/2"/><updated>2026-11-19T09:00:00Z</updated></entry>
</feed>"#;

    const OCDS: &str = r#"{"uri":"x","releases":[
      {"ocid":"ocds-1","date":"2026-11-20T09:00:00Z","buyer":{"name":"City"},
       "tender":{"id":"T-1","title":"Road works","tenderPeriod":{"endDate":"2026-12-20T12:00:00Z"},
         "items":[{"classification":{"scheme":"CPV","id":"45233140"}}],
         "documents":[{"url":"https://portal.example/d/1.pdf"}]}},
      {"ocid":"ocds-2","date":"2026-11-19T09:00:00Z","tender":{"id":"T-2","title":"Meals"}}],
      "links":{"next":"https://portal.example/api/ocds?page=2"}}"#;

    #[test]
    fn rss_2_feed_reports_entries_and_fields() {
        let feed = listing(RSS, "application/rss+xml");
        assert_eq!(feed.method, ConnectionMethod::Feed);
        assert_eq!(feed.items, 2);
        assert_eq!(
            feed.fields,
            ["title", "publicationDate", "documents", "cpv", "id"]
        );
        assert!(!feed.has_pagination);
    }

    #[test]
    fn atom_feed_reports_next_link_as_pagination() {
        let feed = listing(ATOM, "application/atom+xml");
        assert_eq!(feed.method, ConnectionMethod::Feed);
        assert_eq!(feed.items, 2);
        assert_eq!(feed.fields, ["title", "publicationDate", "id"]);
        assert!(feed.has_pagination);
    }

    #[test]
    fn ocds_release_package_maps_fields_and_next_link() {
        let ocds = listing(OCDS, "application/json");
        assert_eq!(ocds.method, ConnectionMethod::Ocds);
        assert_eq!(ocds.items, 2);
        assert_eq!(
            ocds.fields,
            [
                "title",
                "deadline",
                "buyer",
                "publicationDate",
                "documents",
                "cpv",
                "id"
            ]
        );
        assert!(ocds.has_pagination);
    }

    #[test]
    fn plain_json_finds_the_nested_items_array() {
        let body = r#"{"meta":{"tags":[{"a":1}]},"total_count":250,
          "data":{"results":[
            {"idweb":"26-1","objet":"Travaux","nomacheteur":"Ville","dateparution":"2026-11-20"},
            {"idweb":"26-2","objet":"Repas","nomacheteur":"Ville","dateparution":"2026-11-19",
             "datelimitereponse":"2026-12-01"}]}}"#;
        let json = listing(body, "application/json");
        assert_eq!(json.method, ConnectionMethod::Json);
        assert_eq!(json.items, 2);
        assert_eq!(
            json.fields,
            ["title", "deadline", "buyer", "publicationDate", "id"]
        );
        assert!(json.has_pagination, "total_count exceeds the items shown");
    }

    #[test]
    fn json_without_ocid_is_plain_json_not_ocds() {
        let json = listing(r#"{"releases":[{"title":"x"}]}"#, "application/json");
        assert_eq!(json.method, ConnectionMethod::Json);
    }

    #[test]
    fn sitemaps_with_entities_are_refused() {
        let body = r#"<?xml version="1.0"?>
<!DOCTYPE urlset [<!ENTITY secret SYSTEM "file:///etc/passwd">]>
<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
  <url><loc>https://portal.example/notice/&secret;</loc></url></urlset>"#;
        assert_eq!(parse_body(body, Some("application/xml")), Parsed::Unknown);
        assert!(read_sitemap(body).is_none());
    }

    #[test]
    fn field_names_map_to_the_shared_vocabulary() {
        for (key, field) in [
            ("noticeTitle", "title"),
            ("submissionDeadline", "deadline"),
            ("buyer_name", "buyer"),
            ("publicationDate", "publicationDate"),
            ("publicationNumber", "id"),
            ("documents", "documents"),
            ("cpvCode", "cpv"),
            ("contentId", "id"),
            ("titol", "title"),
            ("dataPublicacio", "publicationDate"),
            ("terminiPresentacioOfertes", "deadline"),
            ("organContractacio", "buyer"),
            ("Sluitingsdatum", "deadline"),
            ("Aanbestedende dienst", "buyer"),
            ("Datum zveřejnění", "publicationDate"),
            ("Lhůta pro podání nabídek", "deadline"),
            ("Název zakázky", "title"),
        ] {
            assert_eq!(field_for_key(key), Some(field), "{key}");
        }
        for unrelated in ["colour", "valid", "bid", "paid"] {
            assert_eq!(field_for_key(unrelated), None, "{unrelated}");
        }
    }

    #[test]
    fn statuses_are_read_from_failures() {
        assert_eq!(
            http_status(&ReadFailure::Incomplete("Portal returned HTTP 403".into())),
            Some(403)
        );
        assert_eq!(
            http_status(&ReadFailure::Temporary("connection refused".into())),
            None
        );
    }

    #[tokio::test]
    async fn html_alternates_machine_links_and_ted_become_candidates() {
        let start = "https://portal.example/tenders";
        let html = r#"<html><head>
          <link rel="alternate" type="application/rss+xml" href="/feed.rss">
          <link rel="alternate" type="application/json" href="https://api.portal.example/v1/notices">
        </head><body><main>
          <a href="/notice/1">Road works</a><a href="/notice/2">Meals</a>
          <a href="/open-data/export.json">Open data export</a>
          <a href="/feedback">Feedback</a>
          <a href="https://ted.europa.eu/en/notice/-/detail/807486-2026">On TED</a>
          <a href="https://ted.europa.eu/en/notice/-/detail/807487-2026">On TED</a>
          <a href="/tenders?page=2">Next</a>
        </main></body></html>"#;
        let source = Canned::default()
            .page(start, "text/html; charset=utf-8", html)
            .page(
                "https://portal.example/feed.rss",
                "application/rss+xml",
                RSS,
            )
            .page(
                "https://api.portal.example/v1/notices",
                "application/json",
                OCDS,
            )
            .failure(
                "https://portal.example/open-data/export.json",
                ReadFailure::Incomplete("Portal returned HTTP 403".into()),
            );
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert_eq!(
            methods(&candidates),
            [
                ConnectionMethod::Feed,
                ConnectionMethod::Ocds,
                ConnectionMethod::Json,
                ConnectionMethod::Ted,
                ConnectionMethod::Html,
            ]
        );
        let feed = &candidates[0];
        assert_eq!(feed.found_via, "link rel=alternate");
        assert_eq!(feed.http_status, 200);
        assert_eq!(feed.sample_items, 2);
        let refused = &candidates[2];
        assert_eq!(refused.found_via, "start page link");
        assert_eq!(
            refused.http_status, 403,
            "a refused route is still reported"
        );
        let ted = &candidates[3];
        assert_eq!(ted.sample_items, 2);
        assert!(ted.entry_url.starts_with("https://ted.europa.eu/"));
        let html = &candidates[4];
        assert_eq!(html.entry_url, start);
        assert_eq!(html.sample_items, 2);
        assert!(html.has_pagination);
        assert!(candidates.iter().all(|candidate| candidate.score == 0.0));
        let requested = source.requested();
        assert!(!requested.iter().any(|url| url.contains("feedback")));
        assert!(!requested.iter().any(|url| url.contains("ted.europa.eu")));
    }

    #[tokio::test]
    async fn html_listing_labels_fill_fields_seen() {
        let start = "https://portal.example/aankondigingen";
        let html = r#"<html><body><table>
          <thead><tr><th>Titel</th><th>Aanbestedende dienst</th><th>Publicatiedatum</th>
            <th>Sluitingsdatum</th></tr></thead>
          <tr><td><a href="/aankondiging/1">Wegonderhoud</a></td><td>Gemeente</td></tr>
          <tr><td><a href="/aankondiging/2">Schoolmaaltijden</a></td><td>Provincie</td></tr>
        </table><a href="/files/bestek.pdf">Bestek</a></body></html>"#;
        let source = Canned::default().page(start, "text/html", html);
        let candidates = probe_with(&url(start), &source).await.unwrap();
        let html = candidates.last().unwrap();
        assert_eq!(html.method, ConnectionMethod::Html);
        assert_eq!(
            html.fields_seen,
            ["title", "deadline", "buyer", "publicationDate", "documents"]
        );
    }

    #[tokio::test]
    async fn shell_rejects_error_and_metadata_objects_and_caps_endpoint_samples() {
        let start = "https://portal.example/";
        let source = Canned::default()
            .page(
                start,
                "text/html",
                r#"<div id="app"></div><script src="/main.js"></script>"#,
            )
            .page(
                "https://portal.example/main.js",
                "application/javascript",
                r#"
                fetch('/api/notices/a'); fetch('/api/notices/b');
                fetch('/api/notices/c'); fetch('/api/notices/d');
            "#,
            )
            .page(
                "https://portal.example/api/notices/a",
                "application/json",
                r#"{"data":null,"errors":[{"id":"E"}]}"#,
            )
            .page(
                "https://portal.example/api/notices/b",
                "application/json",
                r#"{"id":"config","title":"Portal"}"#,
            )
            .failure(
                "https://portal.example/api/notices/c",
                ReadFailure::Incomplete("Portal returned HTTP 405".into()),
            );
        let error = probe_with(&url(start), &source).await.unwrap_err();
        let requests = source.requested();
        assert_eq!(
            requests
                .iter()
                .filter(|u| u.contains("/api/notices/"))
                .count(),
            3
        );
        assert!(!requests.iter().any(|u| u.ends_with("/d")));
        assert!(requests.len() <= REQUEST_BUDGET);
        assert!(
            error.to_string().contains("POST search is unsupported"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn app_shell_reads_the_home_page_for_feeds() {
        let start = "https://portal.example/aankondigingen/overzicht";
        let shell = r#"<!doctype html><html><head><base href="/aankondigingen/"></head>
          <body><app-root></app-root><script src="main.js"></script></body></html>"#;
        let source = Canned::default()
            .page(start, "text/html", shell)
            .page(
                "https://portal.example/",
                "text/html",
                r#"<html><head><meta http-equiv="refresh" content="0; url='/cms" /></head></html>"#,
            )
            .page(
                "https://portal.example/cms",
                "text/html",
                r#"<html><body><footer>
                  <a href="https://portal.example/papi/rss/laatste-publicatie.rss">RSS</a>
                </footer></body></html>"#,
            )
            .page(
                "https://portal.example/papi/rss/laatste-publicatie.rss",
                "application/atom+xml",
                ATOM,
            );
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert_eq!(
            methods(&candidates),
            [ConnectionMethod::Feed, ConnectionMethod::Html]
        );
        assert_eq!(candidates[0].found_via, "home page link");
        assert_eq!(candidates[0].sample_items, 2);
        assert_eq!(candidates[1].entry_url, start);
    }

    #[tokio::test]
    async fn govuk_pages_offer_their_content_api() {
        let start = "https://www.gov.uk/government/collections/tenders";
        let html = r#"<html><head><meta name="govuk:format" content="document_collection">
          </head><body><main><a href="/government/publications/a">A</a></main></body></html>"#;
        let api = r#"{"title":"Tenders","links":{"organisations":[{"title":"Agency"}],
          "documents":[
            {"title":"Crime contract","base_path":"/a","content_id":"1","public_updated_at":"2026-11-16T14:32:46Z"},
            {"title":"Civil contract","base_path":"/b","content_id":"2","public_updated_at":"2026-10-29T12:07:06Z"}]}}"#;
        let source = Canned::default().page(start, "text/html", html).page(
            "https://www.gov.uk/api/content/government/collections/tenders",
            "application/json",
            api,
        );
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert_eq!(
            methods(&candidates),
            [ConnectionMethod::Json, ConnectionMethod::Html]
        );
        let json = &candidates[0];
        assert_eq!(json.found_via, "GOV.UK content API");
        assert_eq!(json.sample_items, 2);
        assert_eq!(json.fields_seen, ["title", "id"]);
        assert!(
            !source
                .requested()
                .contains(&"https://www.gov.uk/".to_owned()),
            "the home page is only read when the start page offers nothing"
        );
    }

    #[test]
    fn meta_refresh_targets_are_read() {
        assert_eq!(refresh_target("0; url='/cms").as_deref(), Some("/cms"));
        assert_eq!(refresh_target("5;URL=\"/next\"").as_deref(), Some("/next"));
        assert_eq!(refresh_target("30"), None);
    }

    #[tokio::test]
    async fn sitemap_from_robots_counts_tender_pages() {
        let start = "https://portal.example/";
        let source = Canned::default()
            .page(
                start,
                "text/html",
                "<html><body><p>Welcome</p></body></html>",
            )
            .page(
                "https://portal.example/robots.txt",
                "text/plain",
                "User-agent: *\nSitemap: https://portal.example/sitemap-index.xml # all\n",
            )
            .page(
                "https://portal.example/sitemap-index.xml",
                "application/xml",
                r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                  <sitemap><loc>https://portal.example/sitemap-pages.xml</loc></sitemap>
                  <sitemap><loc>https://portal.example/sitemap-tenders.xml.gz</loc></sitemap>
                </sitemapindex>"#,
            )
            .page(
                "https://portal.example/sitemap-tenders.xml.gz",
                "application/xml",
                r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                  <url><loc>https://portal.example/tender/1</loc></url>
                  <url><loc>https://portal.example/tender/2</loc></url>
                  <url><loc>https://portal.example/about</loc></url>
                </urlset>"#,
            );
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert_eq!(
            methods(&candidates),
            [ConnectionMethod::Sitemap, ConnectionMethod::Html]
        );
        let sitemap = &candidates[0];
        assert_eq!(
            sitemap.entry_url,
            "https://portal.example/sitemap-index.xml"
        );
        assert_eq!(sitemap.found_via, "robots.txt");
        assert_eq!(sitemap.sample_items, 2);
        assert!(sitemap.has_pagination);
        assert!(
            !source
                .requested()
                .iter()
                .any(|url| url.ends_with("sitemap-pages.xml")),
            "the tender child sitemap is preferred"
        );
    }

    #[tokio::test]
    async fn a_page_without_options_still_offers_html() {
        let start = "https://portal.example/";
        let source = Canned::default().page(start, "text/html", "<html><body>Hello</body></html>");
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert_eq!(methods(&candidates), [ConnectionMethod::Html]);
        assert_eq!(candidates[0].http_status, 200);
        assert_eq!(candidates[0].found_via, "start page");
        // Start page, robots.txt and /sitemap.xml.
        assert_eq!(source.requested().len(), 3);
    }

    #[tokio::test]
    async fn refused_start_page_still_reads_the_sitemap() {
        let start = "https://portal.example/search";
        let source = Canned::default()
            .failure(
                start,
                ReadFailure::Incomplete("Portal returned HTTP 403".into()),
            )
            .page(
                "https://portal.example/sitemap.xml",
                "application/xml",
                r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                  <url><loc>https://portal.example/notice/1</loc></url></urlset>"#,
            );
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert_eq!(
            methods(&candidates),
            [ConnectionMethod::Sitemap, ConnectionMethod::Html]
        );
        assert_eq!(candidates[0].found_via, "/sitemap.xml");
        assert_eq!(candidates[1].http_status, 403);
    }

    #[tokio::test]
    async fn refused_start_page_without_alternatives_is_reported() {
        let start = "https://portal.example/";
        let source = Canned::default().failure(
            start,
            ReadFailure::Incomplete("Portal returned HTTP 403".into()),
        );
        let failure = probe_with(&url(start), &source).await.unwrap_err();
        assert_eq!(failure.to_string(), "Portal returned HTTP 403");
    }

    #[tokio::test]
    async fn blocked_or_flaky_start_pages_end_probing() {
        let start = "https://portal.example/";
        for failure in [
            ReadFailure::Blocked("Portal blocks delivery with a CAPTCHA".into()),
            ReadFailure::Temporary("Portal returned HTTP 503".into()),
        ] {
            let source = Canned::default().failure(start, failure.clone());
            let result = probe_with(&url(start), &source).await.unwrap_err();
            assert_eq!(result.to_string(), failure.to_string());
            assert_eq!(source.requested().len(), 1, "no request after a block");
        }
    }

    #[tokio::test]
    async fn probing_stays_within_the_request_budget() {
        let start = "https://portal.example/";
        let mut html = String::from("<html><head>");
        for index in 0..20 {
            html.push_str(&format!(
                r#"<link rel="alternate" type="application/rss+xml" href="/feed{index}.rss">"#
            ));
        }
        html.push_str("</head><body></body></html>");
        let mut source = Canned::default().page(start, "text/html", html).page(
            "https://portal.example/robots.txt",
            "text/plain",
            "",
        );
        for index in 0..20 {
            source = source.page(
                &format!("https://portal.example/feed{index}.rss"),
                "application/rss+xml",
                RSS,
            );
        }
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert!(source.requested().len() <= REQUEST_BUDGET);
        assert_eq!(candidates.last().unwrap().method, ConnectionMethod::Html);
    }

    #[tokio::test]
    async fn a_json_start_page_is_its_own_candidate() {
        let start = "https://portal.example/api/notices";
        let source = Canned::default().page(start, "application/json", OCDS);
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert_eq!(
            methods(&candidates),
            [ConnectionMethod::Ocds, ConnectionMethod::Html]
        );
        assert_eq!(candidates[0].found_via, "start page");
    }

    #[test]
    fn api_endpoints_come_from_links_and_code_samples() {
        let base = url("https://www.portal.example/Developer/Documentation");
        let body = r#"<a href="/apidocumentation/1.0/GET-notices">docs</a>
            <a href="https://data.other.example/api/action/package_search">other host</a>
            <pre>GET https://www.portal.example/api/1.0/ocdsReleasePackages?updatedFrom=2020-12-31</pre>
            <code>/api/1.0/ocdsReleasePackages/000001-2021</code>
            <code>/api/v2/buyers</code> <code>/api/docs/index</code> <code>/api/</code>"#;
        let endpoints: Vec<String> = api_endpoints(&base, body)
            .iter()
            .map(Url::to_string)
            .collect();
        assert_eq!(
            endpoints,
            [
                "https://www.portal.example/api/1.0/ocdsReleasePackages",
                "https://www.portal.example/api/v2/buyers",
            ]
        );
    }

    #[test]
    fn documentation_subpage_prefers_ocds_releases() {
        let base = url("https://portal.example/Developer/Documentation");
        let body = r#"<a href="/apidocumentation/api-how-to-guide">Guide</a>
            <a href="/apidocumentation/1.0/GET-ocdsRecordPackages">Records</a>
            <a href="/apidocumentation/1.0/GET-ocdsReleasePackages">Releases</a>
            <a href="https://elsewhere.example/apidocs/ocds-releases">Elsewhere</a>"#;
        assert_eq!(
            documentation_subpage(&base, body).unwrap().as_str(),
            "https://portal.example/apidocumentation/1.0/GET-ocdsReleasePackages"
        );
        assert!(
            documentation_subpage(&base, r#"<a href="/apidocumentation/terms">T</a>"#).is_none()
        );
    }

    #[tokio::test]
    async fn an_api_link_to_documentation_leads_to_the_endpoint() {
        let start = "https://www.portal.example/Search";
        let source = Canned::default()
            .page(
                start,
                "text/html",
                r#"<html><body><a href="/notice/1">Notice</a>
                <footer><a href="/Developer/Documentation">Data and API</a></footer></body></html>"#,
            )
            .page(
                "https://www.portal.example/Developer/Documentation",
                "text/html",
                r#"<html><body><a href="/apidocumentation/1.0/GET-ocdsReleasePackages">Releases</a></body></html>"#,
            )
            .page(
                "https://www.portal.example/apidocumentation/1.0/GET-ocdsReleasePackages",
                "text/html",
                r#"<html><body><pre>/api/1.0/ocdsReleasePackages?updatedFrom=2020-12-31T23:00:00</pre>
                <pre>/api/1.0/ocdsReleasePackages/000001-2021</pre></body></html>"#,
            )
            .page(
                "https://www.portal.example/api/1.0/ocdsReleasePackages",
                "application/json",
                OCDS,
            );
        let candidates = probe_with(&url(start), &source).await.unwrap();
        assert_eq!(
            methods(&candidates),
            [ConnectionMethod::Ocds, ConnectionMethod::Html]
        );
        assert_eq!(
            candidates[0].entry_url,
            "https://www.portal.example/api/1.0/ocdsReleasePackages"
        );
        assert_eq!(candidates[0].found_via, "API documentation");
        assert!(source.requested().len() <= REQUEST_BUDGET);
    }

    #[tokio::test]
    async fn documentation_is_followed_only_from_api_links() {
        let start = "https://portal.example/";
        let source = Canned::default()
            .page(
                start,
                "text/html",
                r#"<html><body><a href="/rss">RSS feeds</a></body></html>"#,
            )
            .page(
                "https://portal.example/rss",
                "text/html",
                r#"<html><body><code>/api/1.0/notices</code></body></html>"#,
            );
        probe_with(&url(start), &source).await.unwrap();
        assert!(
            !source
                .requested()
                .contains(&"https://portal.example/api/1.0/notices".to_string())
        );
    }

    /// The real client against a local server: gzip sitemaps are unpacked by `PortalHttp`, and
    /// a CAPTCHA page is reported as a block.
    mod over_http {
        use super::*;
        use crate::fetch::Fetcher;
        use axum::{Router, http::header, response::IntoResponse, routing::get};
        use std::io::Write;

        async fn serve(router: Router) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, router).await });
            format!("http://{address}")
        }

        fn gzip(body: &str) -> Vec<u8> {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(body.as_bytes()).unwrap();
            encoder.finish().unwrap()
        }

        #[tokio::test]
        async fn app_shell_bundle_supplies_a_ranked_json_route() {
            let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = calls.clone();
            let router = Router::new()
                .route(
                    "/",
                    get(|| async {
                        axum::response::Html(include_str!("../../tests/fixtures/spa/shell.html"))
                    }),
                )
                .route(
                    "/portal/main.js",
                    get(|| async {
                        (
                            [(header::CONTENT_TYPE, "application/javascript")],
                            include_str!("../../tests/fixtures/spa/main.js"),
                        )
                    }),
                )
                .route(
                    "/portal/api/notices/search",
                    get(|| async {
                        (
                            [(header::CONTENT_TYPE, "application/json")],
                            include_str!("../../tests/fixtures/spa/notices.json"),
                        )
                    }),
                )
                .layer(axum::middleware::from_fn(
                    move |req: axum::extract::Request, next: axum::middleware::Next| {
                        seen.lock().unwrap().push((
                            req.method().clone(),
                            req.uri().to_string(),
                            std::time::Instant::now(),
                        ));
                        async move { next.run(req).await }
                    },
                ));
            let base = serve(router).await;
            let start = url(&base);
            let files = tempfile::tempdir().unwrap();
            let http = Fetcher::new(files.path().into()).unwrap().portal(&start);
            let ranked = crate::discovery::rank_for(&start, probe(&start, &http).await.unwrap());
            let decision = crate::model::top_ranked(ranked, 1).unwrap();
            let chosen = decision.chosen;
            assert_eq!(
                chosen.entry_url,
                format!("{base}/portal/api/notices/search?page=1&limit=5")
            );
            assert_eq!(chosen.method, ConnectionMethod::Json);
            assert!(chosen.score > 0.0 && chosen.has_pagination);
            assert_eq!(chosen.found_via, "app-shell bundle GET");
            assert_eq!((chosen.http_status, chosen.sample_items), (200, 2));
            assert_eq!(chosen.fields_seen, ["title", "deadline", "buyer", "id"]);
            let calls = calls.lock().unwrap();
            assert!(calls.iter().all(|(method, path, _)| method == "GET"
                && !path.contains("advanced")
                && !path.contains("settings")));
            assert!(
                calls
                    .windows(2)
                    .all(|pair| pair[1].2.duration_since(pair[0].2)
                        >= std::time::Duration::from_millis(900))
            );
        }

        #[tokio::test]
        async fn post_only_shell_reports_the_unsupported_method() {
            let router = Router::new()
                .route(
                    "/",
                    get(|| async {
                        axum::response::Html(
                            r#"<div id="app"></div><script src="/main.js"></script>"#,
                        )
                    }),
                )
                .route(
                    "/main.js",
                    get(|| async { "client.post('/api/notices/search', filters)" }),
                );
            let start = url(&serve(router).await);
            let files = tempfile::tempdir().unwrap();
            let http = Fetcher::new(files.path().into()).unwrap().portal(&start);
            let error = probe(&start, &http).await.unwrap_err();
            assert!(
                error.to_string().contains("POST search is unsupported"),
                "{error}"
            );
        }

        /// Mirrors Doffin: an axios instance posts to a path built from a named string constant.
        #[tokio::test]
        async fn templated_post_search_reports_the_unsupported_method() {
            let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let seen = calls.clone();
            let router = Router::new()
                .route(
                    "/search",
                    get(|| async {
                        axum::response::Html(
                            r#"<div id="root"></div><script type="module" src="/assets/index.js"></script>"#,
                        )
                    }),
                )
                .route(
                    "/assets/index.js",
                    get(|| async {
                        (
                            [(header::CONTENT_TYPE, "application/javascript")],
                            // The regex's lone quote desynchronises the literal scanner.
                            "var Id=`search`,Rd=`suggest`,Sf=Td.create({baseURL:jd});\
                             var q=s.replace(/'/g,``);\
                             var r_=async()=>await Sf.post(`/${Id}`,uh(e)).then(e=>e.data);",
                        )
                    }),
                )
                .layer(axum::middleware::from_fn(
                    move |req: axum::extract::Request, next: axum::middleware::Next| {
                        seen.lock().unwrap().push(req.method().clone());
                        async move { next.run(req).await }
                    },
                ));
            let base = serve(router).await;
            let start = Url::parse(&format!("{base}/search")).unwrap();
            let files = tempfile::tempdir().unwrap();
            let http = Fetcher::new(files.path().into()).unwrap().portal(&start);
            let error = probe(&start, &http).await.unwrap_err();
            assert!(
                error.to_string().contains("POST search is unsupported"),
                "{error}"
            );
            assert!(calls.lock().unwrap().iter().all(|m| m == "GET"));
        }

        #[tokio::test]
        async fn scoped_samples_bound_bodies_and_stop_cross_origin_redirects() {
            use axum::response::Redirect;
            let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let seen = hits.clone();
            let other = serve(Router::new().route(
                "/",
                get(move || async move {
                    seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    "unexpected request"
                }),
            ))
            .await;
            let base = serve(
                Router::new()
                    .route("/bundle", get(|| async { "0123456789" }))
                    .route("/gzip", get(|| async { gzip(&"z".repeat(1000)) }))
                    .route(
                        "/redirect",
                        get(move || async move { Redirect::temporary(&other) }),
                    ),
            )
            .await;
            let files = tempfile::tempdir().unwrap();
            let http = Fetcher::new(files.path().into())
                .unwrap()
                .with_cache(files.path().join("scope-cache"))
                .unwrap()
                .portal(&url(&base));
            assert_eq!(
                http.scoped_sample(url(&format!("{base}/bundle")), 4, true)
                    .await
                    .unwrap()
                    .body,
                "0123"
            );
            assert!(
                http.scoped_sample(url(&format!("{base}/bundle")), 4, false)
                    .await
                    .is_err()
            );
            assert_eq!(
                http.scoped_sample(url(&format!("{base}/gzip")), 64, true)
                    .await
                    .unwrap()
                    .body
                    .len(),
                64
            );
            assert!(
                http.scoped_sample(url(&format!("{base}/redirect")), 64, true)
                    .await
                    .is_err()
            );
            assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
            // A truncated sample must not replace a complete response in the development cache.
            assert_eq!(
                http.page(url(&format!("{base}/bundle")))
                    .await
                    .unwrap()
                    .body,
                "0123456789"
            );
            // A redirect chain cached by an ordinary read still has to obey bundle scope.
            http.page(url(&format!("{base}/redirect"))).await.unwrap();
            assert!(
                http.scoped_sample(url(&format!("{base}/redirect")), 64, true)
                    .await
                    .is_err()
            );
            assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        }

        #[tokio::test]
        async fn gzip_sitemap_index_through_the_portal_client() {
            let router = Router::new()
                .route(
                    "/",
                    get(|| async { axum::response::Html("<html><body>Hi</body></html>") }),
                )
                .route(
                    "/robots.txt",
                    get(|| async { "Sitemap: /sitemap_index.xml\n" }),
                )
                .route(
                    "/sitemap_index.xml",
                    get(|| async {
                        (
                            [(header::CONTENT_TYPE, "application/xml")],
                            r#"<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                              <sitemap><loc>/notices.xml.gz</loc></sitemap></sitemapindex>"#,
                        )
                    }),
                )
                .route(
                    "/notices.xml.gz",
                    get(|| async {
                        (
                            [(header::CONTENT_TYPE, "application/gzip")],
                            gzip(
                                r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
                                <url><loc>http://127.0.0.1/notice/1</loc></url>
                                <url><loc>http://127.0.0.1/notice/2</loc></url>
                                <url><loc>http://127.0.0.1/notice/3</loc></url></urlset>"#,
                            ),
                        )
                            .into_response()
                    }),
                );
            let base = serve(router).await;
            let start = url(&format!("{base}/"));
            let files = tempfile::tempdir().unwrap();
            let http = Fetcher::new(files.path().into()).unwrap().portal(&start);
            let candidates = probe(&start, &http).await.unwrap();
            assert_eq!(
                methods(&candidates),
                [ConnectionMethod::Sitemap, ConnectionMethod::Html]
            );
            assert_eq!(candidates[0].sample_items, 3);
            assert_eq!(candidates[0].http_status, 200);
            assert_eq!(
                candidates[0].content_type.as_deref(),
                Some("application/xml")
            );
        }

        #[tokio::test]
        async fn captcha_start_page_is_a_block() {
            let router = Router::new().route(
                "/",
                get(|| async {
                    axum::response::Html(
                        "<html><body><form id=\"challenge-form\">Verify you are human \
                         to continue</form></body></html>",
                    )
                }),
            );
            let base = serve(router).await;
            let start = url(&format!("{base}/"));
            let files = tempfile::tempdir().unwrap();
            let http = Fetcher::new(files.path().into()).unwrap().portal(&start);
            let failure = probe(&start, &http).await.unwrap_err();
            assert!(matches!(failure, ReadFailure::Blocked(_)), "{failure:?}");
            assert_eq!(failure.to_string(), "Portal blocks delivery with a CAPTCHA");
        }
    }
}
