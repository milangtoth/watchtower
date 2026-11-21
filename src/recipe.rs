use crate::{
    domain::{
        CandidateRead, ConnectionCandidate, ConnectionMethod, Document, Evidence, Nature, Place,
        Procedure, ReadFailure, Tender, TenderStatus,
    },
    fetch::{DocumentResponse, Page, PortalHttp, public_http},
    formats, metadata,
};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use chrono_tz::Tz;
use reqwest::Url;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Html,
    Json,
    /// RSS or Atom: each entry is a row with its title and link.
    Feed,
    /// XML sitemap or sitemap index: each URL is a tender detail page.
    Sitemap,
}

/// A listing structure read with fixed rules instead of model-written selectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    /// OCDS release or record packages, with `links.next` pagination.
    Ocds,
}

/// Listing pages read at most in one read.
const LISTING_PAGE_LIMIT: usize = 250;

/// Seconds to wait after an OCDS API answers a later page with something other than JSON.
const OCDS_THROTTLE_WAIT: u64 = 300;

/// Days of releases an OCDS preset read asks for. Open tenders are published or updated within
/// this window, so the read does not page through the publisher's whole history.
const OCDS_WINDOW_DAYS: i64 = 90;

/// The largest page the Find a Tender and Contracts Finder OCDS APIs return.
const OCDS_PAGE_SIZE: &str = "100";

/// More URLs than this in a sitemap would take too long to read at one request per second.
const SITEMAP_PAGE_LIMIT: usize = 500;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Select {
    pub path: String,
    pub attribute: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub start_url: Option<String>,
    pub listing: Format,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    #[serde(default, deserialize_with = "row_path")]
    pub items: String,
    pub next: Option<Select>,
    pub total: Option<Select>,
    pub empty: Option<Select>,
    pub detail_link: Option<Select>,
    pub detail_format: Option<Format>,
    #[serde(default)]
    pub list_fields: BTreeMap<String, Select>,
    #[serde(default)]
    pub detail_fields: BTreeMap<String, Select>,
    pub document_page: Option<Select>,
    pub document_format: Option<Format>,
    pub documents: Option<Select>,
    pub direct_documents: Option<Select>,
    pub nested_document_pages: Option<Select>,
    pub timezone: Option<String>,
    pub date_format: Option<String>,
    pub deadline_format: Option<String>,
    /// Feed and sitemap only: keep entry URLs matching this pattern, where `*` matches any run
    /// of characters. A pattern without `*` matches URLs that contain it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_pattern: Option<String>,
    /// HTML and JSON only: the query parameter holding the page number, for listings that page
    /// by number instead of linking to the next page. The start URL must include it. Reading
    /// stops at an empty page or once the listing total is reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_parameter: Option<String>,
}

/// Reads `items` as a plain path. Models sometimes write it like the other selections, as
/// `{"path": "..."}`. That form is accepted when it carries no attribute, because rows are
/// elements or array entries rather than attribute values. The stored recipe keeps the string form.
fn row_path<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RowPath {
        Path(String),
        Select(Select),
    }
    match RowPath::deserialize(deserializer).map_err(|_| {
        serde::de::Error::custom(
            "items must be a CSS selector or JSON pointer string, or {\"path\": ...}",
        )
    })? {
        RowPath::Path(path) => Ok(path),
        RowPath::Select(Select {
            path,
            attribute: None,
        }) => Ok(path),
        RowPath::Select(Select {
            path,
            attribute: Some(attribute),
        }) if attribute.trim().is_empty() => Ok(path),
        RowPath::Select(_) => Err(serde::de::Error::custom(
            "items selects listing rows and takes no attribute",
        )),
    }
}

const FIELDS: &[&str] = &[
    "id",
    "url",
    "title",
    "buyerName",
    "publicationDate",
    "submissionDeadline",
    "status",
    "referenceNumber",
    "description",
    "procedureType",
    "contractNature",
    "cpvCodes",
    "postalCode",
    "city",
    "countryCode",
];

impl Recipe {
    /// The recipe for a connection method that needs no model-written rules, if any. OCDS maps
    /// its fields directly and starts at the candidate's entry URL.
    pub fn preset(candidate: &ConnectionCandidate) -> Option<Self> {
        (candidate.method == ConnectionMethod::Ocds).then(|| {
            serde_json::from_value(serde_json::json!({
                "start_url": candidate.entry_url, "listing": "json", "preset": "ocds"
            }))
            .expect("the OCDS preset recipe is valid")
        })
    }

    /// The format of detail pages. Feeds and sitemaps link to HTML pages unless set otherwise.
    pub fn page_format(&self) -> Format {
        self.detail_format.unwrap_or(match self.listing {
            Format::Feed | Format::Sitemap => Format::Html,
            format => format,
        })
    }

    /// Whether rows come from a feed or sitemap, which supplies the detail URL itself.
    fn entry_listing(&self) -> bool {
        matches!(self.listing, Format::Feed | Format::Sitemap)
    }

    /// Feed entries open their detail page only when the recipe reads something from it.
    fn feed_details(&self) -> bool {
        !self.detail_fields.is_empty()
            || self.direct_documents.is_some()
            || self.document_page.is_some()
            || self.documents.is_some()
    }

    fn matches_pattern(&self, url: &Url) -> bool {
        self.url_pattern
            .as_deref()
            .is_none_or(|pattern| glob(pattern, url.as_str()))
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(Preset::Ocds) = self.preset {
            ensure!(
                self.listing == Format::Json,
                "the OCDS preset reads JSON listings"
            );
            return Ok(());
        }
        for (name, format) in [
            ("detail_format", self.detail_format),
            ("document_format", self.document_format),
        ] {
            ensure!(
                !matches!(format, Some(Format::Feed | Format::Sitemap)),
                "{name} must be html or json"
            );
        }
        if self.entry_listing() {
            ensure!(
                self.list_fields.is_empty(),
                "feed and sitemap entries supply url and title; use detail_fields for the rest"
            );
        } else {
            ensure!(
                self.url_pattern.is_none(),
                "url_pattern applies to feed and sitemap listings"
            );
        }
        if let Some(parameter) = &self.page_parameter {
            ensure!(
                !parameter.trim().is_empty(),
                "page_parameter names a query parameter"
            );
            ensure!(
                matches!(self.listing, Format::Html | Format::Json),
                "page_parameter applies to html and json listings"
            );
            ensure!(
                self.next.is_none(),
                "use either next or page_parameter for pagination, not both"
            );
        }
        if matches!(self.listing, Format::Html) {
            ensure!(
                !self.items.trim().is_empty(),
                "items needs a nonempty CSS selector for HTML rows"
            );
        }
        validate_path(self.listing, &self.items).context("items")?;
        let detail_format = self.page_format();
        for (name, format, fields) in [
            ("list_fields", self.listing, &self.list_fields),
            ("detail_fields", detail_format, &self.detail_fields),
        ] {
            for (field, rule) in fields {
                ensure!(
                    FIELDS.contains(&field.as_str()),
                    "unknown tender field: {field}"
                );
                validate_path(format, &rule.path).with_context(|| format!("{name}.{field}"))?;
            }
        }
        for (name, rule) in [
            ("next", &self.next),
            ("total", &self.total),
            ("empty", &self.empty),
            ("detail_link", &self.detail_link),
        ] {
            if let Some(rule) = rule {
                validate_path(self.listing, &rule.path).with_context(|| name)?;
            }
        }
        let tender_format = if self.detail_link.is_some() || self.entry_listing() {
            detail_format
        } else {
            self.listing
        };
        if let Some(rule) = &self.document_page {
            validate_path(tender_format, &rule.path).context("document_page")?;
        }
        if let Some(rule) = &self.direct_documents {
            validate_path(tender_format, &rule.path).context("direct_documents")?;
        }
        let document_format = self.document_format.unwrap_or(Format::Html);
        if let Some(rule) = &self.nested_document_pages {
            ensure!(
                self.document_page.is_some(),
                "nested document pages need a document page selector"
            );
            validate_path(document_format, &rule.path).context("nested_document_pages")?;
        }
        if let Some(rule) = &self.documents {
            let format = if self.document_page.is_some() {
                document_format
            } else {
                tender_format
            };
            validate_path(format, &rule.path).context("documents")?;
        }
        if let Some(timezone) = &self.timezone {
            ensure!(timezone.parse::<Tz>().is_ok(), "invalid IANA timezone");
        }
        ensure!(
            self.detail_fields.is_empty() || self.detail_link.is_some() || self.entry_listing(),
            "detail fields need a detail link"
        );
        ensure!(
            self.document_page.is_none() || self.documents.is_some(),
            "document page needs a document selector"
        );
        Ok(())
    }
}

fn validate_path(format: Format, path: &str) -> Result<()> {
    match format {
        Format::Html if !path.is_empty() => {
            Selector::parse(path).map_err(|err| anyhow::anyhow!("invalid CSS selector: {err}"))?;
        }
        Format::Json => ensure!(
            path.is_empty() || path.starts_with('/'),
            "invalid JSON pointer"
        ),
        _ => {}
    }
    Ok(())
}

enum Source {
    Html(Html),
    Json(Value),
}

enum Node<'a> {
    Element(ElementRef<'a>),
    Json(&'a Value),
}

impl Source {
    fn parse(format: Format, body: &str) -> Result<Self> {
        match format {
            Format::Html => Ok(Self::Html(Html::parse_document(body))),
            Format::Json => Ok(Self::Json(
                serde_json::from_str(body).context("expected a JSON response")?,
            )),
            Format::Feed | Format::Sitemap => {
                anyhow::bail!("feed and sitemap pages are read by their own parser")
            }
        }
    }

    fn root(&self) -> Node<'_> {
        match self {
            Self::Html(html) => Node::Element(html.root_element()),
            Self::Json(value) => Node::Json(value),
        }
    }

    fn items(&self, path: &str) -> Result<Vec<Node<'_>>> {
        match self {
            Self::Html(html) => {
                let selector = Selector::parse(path)
                    .map_err(|err| anyhow::anyhow!("invalid item selector: {err}"))?;
                Ok(html.select(&selector).map(Node::Element).collect())
            }
            Self::Json(value) => Ok(value
                .pointer(path)
                .and_then(Value::as_array)
                .context("listing JSON pointer no longer identifies an array")?
                .iter()
                .map(Node::Json)
                .collect()),
        }
    }
}

impl Node<'_> {
    fn values(&self, rule: &Select) -> Vec<String> {
        match self {
            Self::Element(node) => {
                let nodes: Vec<_> = if rule.path.is_empty() {
                    vec![*node]
                } else if let Ok(selector) = Selector::parse(&rule.path) {
                    node.select(&selector).collect()
                } else {
                    Vec::new()
                };
                nodes
                    .into_iter()
                    .filter_map(|node| match &rule.attribute {
                        Some(attr) => node.value().attr(attr).map(str::to_owned),
                        None => Some(
                            node.text()
                                .flat_map(str::split_whitespace)
                                .collect::<Vec<_>>()
                                .join(" "),
                        ),
                    })
                    .map(|text| text.trim().to_owned())
                    .filter(|text| !text.is_empty())
                    .collect()
            }
            Self::Json(node) => match node.pointer(&rule.path) {
                Some(Value::Array(values)) => values.iter().filter_map(json_text).collect(),
                Some(value) => json_text(value).into_iter().collect(),
                None => Vec::new(),
            },
        }
    }

    fn first(&self, rule: &Select) -> Option<String> {
        self.values(rule).into_iter().next()
    }
}

fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

type Fields = BTreeMap<String, Vec<String>>;

fn extract(node: &Node<'_>, rules: &BTreeMap<String, Select>) -> Fields {
    rules
        .iter()
        .filter_map(|(name, rule)| {
            let values = node.values(rule);
            (!values.is_empty()).then_some((name.clone(), values))
        })
        .collect()
}

struct ListingRow {
    fields: Fields,
    detail: Option<Url>,
    attachments: Attachments,
}

#[derive(Default)]
struct Attachments {
    pages: Vec<Url>,
    files: Vec<Url>,
    /// The HTML fallback found these links, not recipe selectors.
    fallback: bool,
    scope: Option<String>,
}

struct ListingPage {
    rows: Vec<ListingRow>,
    /// Every next-page link the page offers, in document order. A selector can match
    /// "first", "previous" and "next" links alike; the reader follows the first page not yet read.
    next: Vec<Url>,
    total: Option<usize>,
    /// The page shows the recipe's empty state or a total of 0.
    empty_state: bool,
    links: ListingLinks,
    /// Further listing pages to read, such as the child sitemaps of a sitemap index.
    more: Vec<Url>,
}

#[derive(Default)]
struct ListingLinks {
    /// Every same-origin link on an HTML listing page, without fragments.
    all: HashSet<String>,
    /// Links marked as pagination: `rel=next`, or a next, pager or pagination class, ID or
    /// label on the link or a close ancestor.
    pagination: Vec<Url>,
}

const UNFOLLOWED_LINK_LIMIT: usize = 20;

pub async fn read(
    http: &PortalHttp,
    portal_url: &Url,
    recipe: &Recipe,
) -> Result<CandidateRead, ReadFailure> {
    recipe.validate().map_err(incomplete)?;
    let mut start = match &recipe.start_url {
        Some(start) => resolve(portal_url, start).map_err(incomplete)?,
        None => portal_url.clone(),
    };
    if recipe.preset == Some(Preset::Ocds) {
        start = ocds_window(&start, chrono::Utc::now());
    }
    let mut queue = VecDeque::from([start]);
    let mut seen_pages = HashSet::new();
    let mut listing_pages = Vec::new();
    let mut fetched = HashSet::new();
    let mut listing_links = HashSet::new();
    let mut pagination_links = Vec::new();
    let mut listing_rows = Vec::new();
    let mut samples = Vec::new();
    let mut total = None;
    let mut empty_state = false;
    while let Some(mut url) = queue.pop_front() {
        url.set_fragment(None);
        if !seen_pages.insert(url.clone()) {
            continue;
        }
        if seen_pages.len() > LISTING_PAGE_LIMIT {
            return Err(ReadFailure::Incomplete(format!(
                "The listing has more than {LISTING_PAGE_LIMIT} pages; use a listing limited to open tenders"
            )));
        }
        listing_pages.push(url.to_string());
        fetched.insert(url.to_string());
        let page = http.page(url.clone()).await?;
        fetched.insert(without_fragment(&page.url));
        if samples.len() < 3 {
            samples.push(sample_page(&page, 16_000).body);
        }
        // An OCDS API that answers a later page with something other than JSON is usually
        // limiting requests; the recipe that read the earlier pages is not broken.
        if recipe.preset == Some(Preset::Ocds)
            && listing_pages.len() > 1
            && serde_json::from_str::<serde::de::IgnoredAny>(&page.body).is_err()
        {
            return Err(ReadFailure::Throttled {
                reason: format!(
                    "OCDS page {} at {} was not JSON; the publisher is probably limiting requests",
                    listing_pages.len(),
                    location(&page.url)
                ),
                retry_after: std::time::Duration::from_secs(OCDS_THROTTLE_WAIT),
            });
        }
        let listing = parse_listing(&page, recipe).map_err(incomplete)?;
        if let Some(count) = listing.total {
            if total.is_some_and(|previous| previous != count) {
                return Err(ReadFailure::Incomplete(
                    "Listing total changed during pagination".into(),
                ));
            }
            total = Some(count);
        }
        empty_state |= listing.empty_state;
        listing_links.extend(listing.links.all);
        pagination_links.extend(listing.links.pagination);
        let page_rows = listing.rows.len();
        listing_rows.extend(listing.rows);
        let next = match &recipe.page_parameter {
            Some(parameter) => {
                let finished =
                    page_rows == 0 || total.is_some_and(|count| listing_rows.len() >= count);
                if finished {
                    None
                } else {
                    Some(next_page_number(&url, parameter).map_err(incomplete)?)
                }
            }
            None => listing.next.into_iter().find_map(|mut next| {
                next.set_fragment(None);
                (!fetched.contains(next.as_str())).then_some(next)
            }),
        };
        queue.extend(next);
        queue.extend(listing.more);
    }
    if recipe.listing == Format::Sitemap {
        let mut details = HashSet::new();
        listing_rows.retain(|row| {
            row.detail
                .as_ref()
                .is_none_or(|url| details.insert(url.clone()))
        });
        if listing_rows.len() > SITEMAP_PAGE_LIMIT {
            return Err(ReadFailure::Incomplete(format!(
                "The sitemap lists {} matching pages, more than {SITEMAP_PAGE_LIMIT}; narrow url_pattern to tender pages",
                listing_rows.len()
            )));
        }
    }
    if total.is_some_and(|count| count != listing_rows.len()) {
        return Err(ReadFailure::Incomplete(
            "Read did not match the source listing total".into(),
        ));
    }
    let end_of_list = if listing_rows.is_empty() && empty_state {
        "empty"
    } else if total.is_some() {
        "total"
    } else if recipe.total.is_some() {
        return Err(ReadFailure::Incomplete(
            "Listing total selector matched nothing, so the read cannot show it is complete".into(),
        ));
    } else {
        "last-page"
    };
    let mut unfollowed_links: Vec<String> = pagination_links
        .iter()
        .map(without_fragment)
        .filter(|link| !fetched.contains(link))
        .collect();
    unfollowed_links.sort();
    unfollowed_links.dedup();
    unfollowed_links.truncate(UNFOLLOWED_LINK_LIMIT);
    let entries = listing_rows.len();
    let mut tenders = Vec::with_capacity(entries);
    let mut identities = HashSet::new();
    let mut document_count = 0;
    for mut row in listing_rows {
        if let Some(url) = row.detail {
            let page = http.page(url).await?;
            let (fields, attachments) = parse_detail(&page, recipe).map_err(incomplete)?;
            row.fields.extend(fields);
            row.fields
                .entry("url".into())
                .or_insert_with(|| vec![page.url.to_string()]);
            row.attachments = attachments;
            if samples.len() < 4 {
                samples.push(sample_page(&page, 16_000).body);
            }
        }
        let mut tender = normalize(row.fields, recipe);
        let key = tender.key().ok_or_else(|| {
            ReadFailure::Incomplete(
                "A listing item has neither a source ID nor a detail URL".into(),
            )
        })?;
        if !identities.insert(key) {
            return Err(ReadFailure::Incomplete(
                "Pagination produced duplicate tender identities".into(),
            ));
        }
        let referrer = tender.url.as_deref().and_then(|url| Url::parse(url).ok());
        tender.documents =
            attachment_files(http, row.attachments, recipe, referrer, &mut samples).await?;
        tender.documents.sort_by(|left, right| {
            (&left.file_name, &left.sha256).cmp(&(&right.file_name, &right.sha256))
        });
        tender.documents.dedup();
        document_count += tender.documents.len();
        tenders.push(tender);
    }
    Ok(CandidateRead {
        tenders,
        evidence: Evidence {
            pages: seen_pages.len(),
            entries,
            terminal_page: true,
            end_of_list,
            total,
            empty_state,
            document_count,
            listing_pages,
            unfollowed_links,
            samples,
            listing_links,
        },
    })
}

fn parse_listing(page: &Page, recipe: &Recipe) -> Result<ListingPage> {
    if recipe.preset == Some(Preset::Ocds) {
        return ocds_listing(page);
    }
    match recipe.listing {
        Format::Feed => return feed_listing(page, recipe),
        Format::Sitemap => return sitemap_listing(page, recipe),
        Format::Html | Format::Json => {}
    }
    let source = Source::parse(recipe.listing, &page.body)
        .map_err(|error| wrong_format(error, recipe.listing, page, "Listing"))?;
    let root = source.root();
    let next = resolve_many(&root, recipe.next.as_ref(), &page.url)?;
    let total = match recipe.total.as_ref().and_then(|rule| root.first(rule)) {
        Some(text) => Some(parse_total(&text).ok_or_else(|| {
            anyhow::anyhow!(
                "Listing total no longer parses as a count: {:?}",
                sample(&text, 80)
            )
        })?),
        None => None,
    };
    let empty = recipe
        .empty
        .as_ref()
        .is_some_and(|rule| root.first(rule).is_some());
    let items = source.items(&recipe.items)?;
    if items.is_empty() {
        let json_empty = matches!(recipe.listing, Format::Json) && next.is_empty();
        // A numbered page past the end is often just empty.
        let past_end = recipe.page_parameter.is_some();
        ensure!(
            empty || json_empty || past_end || total == Some(0),
            "Listing yielded no rows and has no recognized empty state"
        );
    }
    let mut rows = Vec::with_capacity(items.len());
    for item in items {
        let detail = resolve_one(&item, recipe.detail_link.as_ref(), &page.url)?;
        ensure!(
            recipe.detail_link.is_none() || detail.is_some(),
            "Listing item lost its detail link"
        );
        let mut fields = extract(&item, &recipe.list_fields);
        resolve_field_url(&mut fields, &page.url);
        let attachments = if detail.is_some() {
            Attachments::default()
        } else {
            find_documents(&item, recipe, &page.url)?
        };
        rows.push(ListingRow {
            fields,
            detail,
            attachments,
        });
    }
    Ok(ListingPage {
        rows,
        next,
        total,
        empty_state: empty || total == Some(0),
        links: listing_links(&source, &page.url),
        more: Vec::new(),
    })
}

fn feed_listing(page: &Page, recipe: &Recipe) -> Result<ListingPage> {
    let feed = formats::feed(&page.body)?;
    let next = feed
        .next
        .as_deref()
        .map(|next| resolve(&page.url, next))
        .transpose()?;
    let empty_state = feed.entries.is_empty() && next.is_none();
    let mut rows = Vec::with_capacity(feed.entries.len());
    for entry in feed.entries {
        let mut fields = Fields::new();
        let mut detail = None;
        if let Some(link) = entry.link {
            let link = resolve(&page.url, &link)?;
            if !recipe.matches_pattern(&link) {
                continue;
            }
            fields.insert("url".into(), vec![link.to_string()]);
            detail = recipe.feed_details().then_some(link);
        }
        if let Some(title) = entry.title {
            fields.insert("title".into(), vec![title]);
        }
        rows.push(ListingRow {
            fields,
            detail,
            attachments: Attachments::default(),
        });
    }
    Ok(ListingPage {
        rows,
        next: next.into_iter().collect(),
        total: None,
        empty_state,
        links: ListingLinks::default(),
        more: Vec::new(),
    })
}

fn sitemap_listing(page: &Page, recipe: &Recipe) -> Result<ListingPage> {
    let map = formats::sitemap(&page.body)?;
    let mut rows = Vec::new();
    for location in map.urls {
        let url = resolve(&page.url, &location)?;
        if !recipe.matches_pattern(&url) {
            continue;
        }
        rows.push(ListingRow {
            fields: Fields::from([("url".into(), vec![url.to_string()])]),
            detail: Some(url),
            attachments: Attachments::default(),
        });
    }
    let more = map
        .sitemaps
        .iter()
        .map(|location| resolve(&page.url, location))
        .collect::<Result<_>>()?;
    Ok(ListingPage {
        rows,
        next: Vec::new(),
        total: None,
        empty_state: false,
        links: ListingLinks::default(),
        more,
    })
}

fn ocds_listing(page: &Page) -> Result<ListingPage> {
    let package: Value = serde_json::from_str(&page.body).context("expected a JSON response")?;
    let parsed = formats::ocds(&package)?;
    let next = parsed
        .next
        .as_deref()
        .map(|next| resolve(&page.url, next))
        .transpose()?;
    let rows = parsed
        .tenders
        .into_iter()
        .map(|tender| {
            let mut fields = tender.fields;
            resolve_field_url(&mut fields, &page.url);
            let files = tender
                .documents
                .iter()
                .filter_map(|document| resolve(&page.url, document).ok())
                .collect();
            ListingRow {
                fields,
                detail: None,
                attachments: Attachments {
                    files,
                    ..Attachments::default()
                },
            }
        })
        .collect();
    Ok(ListingPage {
        rows,
        next: next.into_iter().collect(),
        total: None,
        empty_state: false,
        links: ListingLinks::default(),
        more: Vec::new(),
    })
}

/// Narrows an OCDS API URL to tender-stage releases published or updated in the last
/// `OCDS_WINDOW_DAYS`, in the largest pages the API allows, using the query parameters of the
/// API family it recognizes. The window ends now, so the API's cursor reaches a last page.
///
/// Find a Tender (`ocdsReleasePackages`, `ocdsRecordPackages`) filters with `updatedFrom`,
/// `updatedTo`, `stages` and `limit` and pages with `cursor`. Contracts Finder (`OCDS/Search`)
/// uses `publishedFrom` and `publishedTo`. Other URLs are read as given. A cursor in the entry
/// URL is dropped so that every read starts at the window's first page.
fn ocds_window(url: &Url, now: chrono::DateTime<chrono::Utc>) -> Url {
    let path = url.path().trim_end_matches('/').to_ascii_lowercase();
    let has = |name: &str| {
        url.query_pairs()
            .any(|(key, _)| key.eq_ignore_ascii_case(name))
    };
    let (from, to) = if path.ends_with("/ocdsreleasepackages")
        || path.ends_with("/ocdsrecordpackages")
        || has("updatedFrom")
        || has("updatedTo")
    {
        ("updatedFrom", "updatedTo")
    } else if path.ends_with("/ocds/search") || has("publishedFrom") || has("publishedTo") {
        ("publishedFrom", "publishedTo")
    } else {
        return url.clone();
    };
    let replaced = [from, to, "stages", "limit", "cursor"];
    let mut pairs: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| !replaced.iter().any(|name| key.eq_ignore_ascii_case(name)))
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let stamp = |time: chrono::DateTime<chrono::Utc>| time.format("%Y-%m-%dT%H:%M:%S").to_string();
    pairs.extend([
        (
            from.to_owned(),
            stamp(now - chrono::Duration::days(OCDS_WINDOW_DAYS)),
        ),
        (to.to_owned(), stamp(now)),
        ("stages".to_owned(), "tender".to_owned()),
        ("limit".to_owned(), OCDS_PAGE_SIZE.to_owned()),
    ]);
    let mut windowed = url.clone();
    windowed.query_pairs_mut().clear().extend_pairs(pairs);
    windowed
}

/// The same URL with its page-number query parameter increased by one.
fn next_page_number(url: &Url, parameter: &str) -> Result<Url> {
    let mut found = false;
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(key, value)| {
            if key == parameter && !found {
                found = true;
                let number: u64 = value.parse().map_err(|_| {
                    anyhow::anyhow!("page_parameter {parameter} is not a number: {value:?}")
                })?;
                Ok((key.into_owned(), (number + 1).to_string()))
            } else {
                Ok((key.into_owned(), value.into_owned()))
            }
        })
        .collect::<Result<_>>()?;
    ensure!(
        found,
        "page_parameter {parameter} is missing from the listing URL; include it in start_url"
    );
    let mut next = url.clone();
    next.query_pairs_mut().clear().extend_pairs(pairs);
    Ok(next)
}

/// Explains a page that is not in the format the recipe expects, which a repair can act on.
fn wrong_format(error: anyhow::Error, format: Format, page: &Page, what: &str) -> anyhow::Error {
    if format == Format::Json && page.body.trim_start().starts_with('<') {
        let hint = if what == "Detail" {
            "set detail_format to \"html\" or point detail_link at the JSON data"
        } else {
            "use listing \"html\" or point start_url at the JSON endpoint"
        };
        anyhow::anyhow!(
            "{what} page {} is HTML, not JSON; {hint}",
            location(&page.url)
        )
    } else {
        error.context(format!("{what} page {}", location(&page.url)))
    }
}

/// Matches `text` against a pattern where `*` stands for any run of characters. A pattern
/// without `*` matches text that contains it.
fn glob(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return text.contains(pattern);
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(index) => rest = &rest[index + part.len()..],
            None => return false,
        }
    }
    true
}

/// Reads a listing total: a plain count, one grouped in thousands such as `1.234` or `1 234`,
/// or such a count at the start of a phrase, as in "8 aanbestedingen gevonden" or
/// "1.234 results". Text without a clear leading count, such as "Page 1 of 2" or "1–20 of 57",
/// is rejected so that a changed total cannot silently become a wrong one.
fn parse_total(text: &str) -> Option<usize> {
    let text = text.trim();
    let (count, rest) = leading_count(text)?;
    if rest.is_empty() {
        return Some(count);
    }
    // A phrase must continue with a space and a word, not with "of 2" or "–20".
    let mut words = rest.split_whitespace();
    let first = words.next()?;
    let separated = rest.starts_with(char::is_whitespace);
    let word = first.chars().next().is_some_and(char::is_alphabetic);
    let range = [
        "of", "van", "von", "de", "du", "sur", "di", "z", "av", "af", "från", "od",
    ]
    .contains(&first.to_lowercase().as_str());
    (separated && word && !range).then_some(count)
}

/// The longest count at the start of `text`, in plain digits or grouped in thousands, with the
/// remaining text.
fn leading_count(text: &str) -> Option<(usize, &str)> {
    let digits = |s: &str| s.bytes().take_while(u8::is_ascii_digit).count();
    let first = digits(text);
    if first == 0 {
        return None;
    }
    let mut end = first;
    if first <= 3 {
        loop {
            let rest = &text[end..];
            let Some(separator) = rest
                .chars()
                .next()
                .filter(|ch| matches!(ch, '.' | ',' | ' ' | '\u{a0}' | '\u{202f}' | '\''))
            else {
                break;
            };
            let group = &rest[separator.len_utf8()..];
            if digits(group) == 3 && !group[3..].starts_with(|ch: char| ch.is_ascii_digit()) {
                end += separator.len_utf8() + 3;
            } else {
                break;
            }
        }
    }
    let count = text[..end]
        .chars()
        .filter(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()?;
    Some((count, &text[end..]))
}

fn without_fragment(url: &Url) -> String {
    let mut url = url.clone();
    url.set_fragment(None);
    url.to_string()
}

fn listing_links(source: &Source, base: &Url) -> ListingLinks {
    let Source::Html(html) = source else {
        return ListingLinks::default();
    };
    let anchors = Selector::parse("a[href]").unwrap();
    let mut links = ListingLinks::default();
    for anchor in html.select(&anchors) {
        let Some(url) = anchor
            .value()
            .attr("href")
            .and_then(|href| base.join(href.trim()).ok())
            .filter(|url| url.origin() == base.origin())
        else {
            continue;
        };
        if pagination_link(anchor) {
            links.pagination.push(url.clone());
        }
        links.all.insert(without_fragment(&url));
    }
    links
}

fn pagination_link(anchor: ElementRef<'_>) -> bool {
    let marked = |element: ElementRef<'_>, terms: &[&str]| {
        ["class", "id", "aria-label", "rel"]
            .into_iter()
            .filter_map(|name| element.value().attr(name))
            .any(|value| {
                let value = value.to_ascii_lowercase();
                terms.iter().any(|term| value.contains(term))
            })
    };
    marked(anchor, &["next", "pager", "pagination"])
        || anchor
            .ancestors()
            .filter_map(ElementRef::wrap)
            .take(3)
            .any(|element| marked(element, &["pager", "pagination"]))
}

/// Reads a tender detail page. Detail pages on one portal can use different templates, so
/// when the recipe selects documents but its selectors match nothing on an HTML detail page,
/// the HTML fallback reads that page's main content instead.
fn parse_detail(page: &Page, recipe: &Recipe) -> Result<(Fields, Attachments)> {
    let format = recipe.page_format();
    let source = Source::parse(format, &page.body)
        .map_err(|error| wrong_format(error, format, page, "Detail"))?;
    let root = source.root();
    let mut fields = extract(&root, &recipe.detail_fields);
    resolve_field_url(&mut fields, &page.url);
    if let Source::Html(document) = &source {
        let content = main_content(document);
        let gov_uk = page.url.host_str() == Some("www.gov.uk");
        if !fields.contains_key("buyerName")
            && let Some(buyer) = metadata::buyer(content, gov_uk)
        {
            fields.insert("buyerName".into(), vec![buyer]);
        }
        if !fields.contains_key("publicationDate")
            && let Some(date) = metadata::publication_date(content)
        {
            fields.insert("publicationDate".into(), vec![date.to_string()]);
        }
        let timezone = recipe
            .timezone
            .as_deref()
            .and_then(|name| name.parse().ok())
            .or_else(|| gov_uk.then_some(chrono_tz::Europe::London));
        if fields
            .get("submissionDeadline")
            .and_then(|values| values.first())
            .and_then(|value| deadline(value, recipe))
            .is_none()
            && let Some(timezone) = timezone
            && let Some(date) = metadata::submission_deadline(content, timezone)
        {
            fields.insert("submissionDeadline".into(), vec![date.to_rfc3339()]);
        }
    }

    let mut attachments = find_documents(&root, recipe, &page.url)?;
    let selects_documents = recipe.document_page.is_some()
        || recipe.direct_documents.is_some()
        || recipe.documents.is_some();
    if matches!(format, Format::Html)
        && selects_documents
        && attachments.pages.is_empty()
        && attachments.files.is_empty()
    {
        attachments = html_attachment_links(page);
        if !attachments.pages.is_empty() || !attachments.files.is_empty() {
            tracing::info!(
                location = %location(&page.url),
                files = attachments.files.len(),
                pages = attachments.pages.len(),
                "Document selectors matched nothing on detail page; using HTML fallback"
            );
        }
    }
    Ok((fields, attachments))
}

fn parse_documents(page: &Page, recipe: &Recipe) -> Result<Attachments> {
    let source = Source::parse(recipe.document_format.unwrap_or(Format::Html), &page.body)?;
    let root = source.root();
    Ok(Attachments {
        pages: resolve_many(&root, recipe.nested_document_pages.as_ref(), &page.url)?,
        files: resolve_many(&root, recipe.documents.as_ref(), &page.url)?,
        fallback: false,
        scope: None,
    })
}

fn find_documents(node: &Node<'_>, recipe: &Recipe, base: &Url) -> Result<Attachments> {
    let pages = resolve_many(node, recipe.document_page.as_ref(), base)?;
    let mut files = resolve_many(node, recipe.direct_documents.as_ref(), base)?;
    if recipe.document_page.is_none() {
        files.extend(resolve_many(node, recipe.documents.as_ref(), base)?);
    }
    Ok(Attachments {
        pages,
        files,
        fallback: false,
        scope: None,
    })
}

/// How an attachment link was found. Recipe selectors were learned on the portal's own tender
/// and attachment pages. The HTML fallback can lead to pages on other publications, where those
/// selectors may match unrelated links such as external applications.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkSource {
    Recipe,
    Fallback,
}

impl LinkSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Recipe => "recipe",
            Self::Fallback => "fallback",
        }
    }
}

struct QueuedLink {
    url: Url,
    depth: usize,
    source: LinkSource,
    referrer: Option<Url>,
    scope: Option<String>,
}

async fn attachment_files(
    http: &PortalHttp,
    attachments: Attachments,
    recipe: &Recipe,
    referrer: Option<Url>,
    samples: &mut Vec<String>,
) -> Result<Vec<Document>, ReadFailure> {
    const MAX_PAGES: usize = 32;
    const MAX_DEPTH: usize = 3;
    let queue = |urls: Vec<Url>, depth, source, referrer: &Option<Url>, scope: Option<&str>| {
        urls.into_iter()
            .map(|url| QueuedLink {
                url,
                depth,
                source,
                referrer: referrer.clone(),
                scope: scope.map(str::to_owned),
            })
            .collect::<Vec<_>>()
    };
    let initial = if attachments.fallback {
        LinkSource::Fallback
    } else {
        LinkSource::Recipe
    };
    let mut pages = VecDeque::from(queue(
        attachments.pages,
        1,
        initial,
        &referrer,
        attachments.scope.as_deref(),
    ));
    let mut files = VecDeque::from(queue(attachments.files, 0, initial, &referrer, None));
    let mut visited = HashSet::new();
    let mut downloaded = HashSet::new();
    let mut page_files = HashSet::new();
    let mut documents = Vec::new();
    let mut fetched = 0;
    // Attachment pages that offered onward links.
    let mut linked_pages = 0;
    while let Some((link, is_page)) = pages
        .pop_front()
        .map(|link| (link, true))
        .or_else(|| files.pop_front().map(|link| (link, false)))
    {
        if visited.contains(&link.url) {
            continue;
        }
        let (page, depth) = if is_page {
            if link.depth > MAX_DEPTH || fetched == MAX_PAGES {
                return Err(ReadFailure::Incomplete(
                    "Attachment page traversal limit reached".into(),
                ));
            }
            let response = http.page_unless_seen(link.url.clone(), &visited).await?;
            visited.insert(link.url.clone());
            let Some(page) = response else { continue };
            tracing::info!(
                location = %location(&page.url),
                source = link.source.as_str(),
                "Inspecting attachment page"
            );
            (page, link.depth)
        } else {
            visited.insert(link.url.clone());
            match http.documents(link.url.clone()).await? {
                DocumentResponse::Files(result) => {
                    downloaded.insert(link.url);
                    documents.extend(result);
                    continue;
                }
                DocumentResponse::Html(page) => {
                    if page.url != link.url && visited.contains(&page.url) {
                        continue;
                    }
                    (page, link.depth + 1)
                }
            }
        };
        if depth > MAX_DEPTH || fetched == MAX_PAGES {
            return Err(ReadFailure::Incomplete(
                "Attachment page traversal limit reached".into(),
            ));
        }
        fetched += 1;
        visited.insert(page.url.clone());
        let found = attachment_page_links(
            &page,
            recipe,
            is_page,
            link.source,
            link.referrer.as_ref(),
            link.scope.as_deref(),
        );
        let (more, source) = match found {
            Ok(found) => found,
            // The fallback guessed this page from its name alone, so a page without file links
            // is not evidence that the recipe broke.
            Err(error)
                if link.source == LinkSource::Fallback
                    && numbered_document_link(&link.url) == Some(NumberedLink::Document) =>
            {
                tracing::warn!(
                    location = %location(&page.url),
                    %error,
                    "Skipping document page without file links"
                );
                continue;
            }
            Err(error) => return Err(incomplete(error)),
        };
        linked_pages += 1;
        page_files.extend(more.files.iter().cloned());
        let here = Some(page.url.clone());
        files.extend(queue(more.files, depth, source, &here, None));
        pages.extend(queue(
            more.pages,
            depth + 1,
            source,
            &here,
            more.scope.as_deref(),
        ));
        if samples.len() < 5 {
            samples.push(sample_page(&page, 16_000).body);
        }
    }
    if linked_pages > 0 && page_files.is_disjoint(&downloaded) {
        return Err(ReadFailure::Incomplete(
            "Attachment pages did not lead to any file links".into(),
        ));
    }
    Ok(documents)
}

/// Finds onward links on an attachment page and reports which rule found them.
///
/// Recipe selectors apply to pages the recipe selected: `document_page` and
/// `nested_document_pages` targets, and HTML returned by a recipe-selected file link when the
/// recipe declares attachment pages. Without `document_page`, `documents` describes the tender
/// row or detail page, so it does not apply to other pages. Pages reached through the HTML
/// fallback use the fallback only.
fn attachment_page_links(
    page: &Page,
    recipe: &Recipe,
    expected_page: bool,
    source: LinkSource,
    referrer: Option<&Url>,
    scope: Option<&str>,
) -> Result<(Attachments, LinkSource)> {
    let json = matches!(recipe.document_format, Some(Format::Json));
    let use_recipe = source == LinkSource::Recipe
        && (expected_page || (recipe.document_page.is_some() && !json));
    let mut found = if use_recipe {
        parse_documents(page, recipe)?
    } else {
        Attachments::default()
    };
    let mut found_by = LinkSource::Recipe;
    if found.pages.is_empty() && found.files.is_empty() && !(use_recipe && json) {
        found = scoped_html_attachment_links(page, scope);
        found_by = LinkSource::Fallback;
    }
    ensure!(
        !found.pages.is_empty() || !found.files.is_empty(),
        "No attachment links matched on document page {}{}",
        location(&page.url),
        referrer
            .map(|url| format!(", linked from {}", location(url)))
            .unwrap_or_default()
    );
    Ok((found, found_by))
}

/// Host and path of a URL for logs and failure reasons, without its query string.
fn location(url: &Url) -> String {
    let path: String = url.path().chars().take(240).collect();
    format!("{}{path}", url.host_str().unwrap_or_default())
}

fn html_attachment_links(page: &Page) -> Attachments {
    scoped_html_attachment_links(page, None)
}

fn scoped_html_attachment_links(page: &Page, scope: Option<&str>) -> Attachments {
    let document = Html::parse_document(&page.body);
    let content = main_content(&document);
    let anchors = Selector::parse("a[href]").unwrap();
    let mut candidates = BTreeMap::new();
    let mut files = Vec::new();
    let mut document_pages = Vec::new();
    for anchor in content.select(&anchors) {
        let Some(href) = anchor
            .value()
            .attr("href")
            .filter(|href| !href.trim().is_empty())
        else {
            continue;
        };
        let Ok(url) = resolve(&page.url, href) else {
            continue;
        };
        if url == page.url {
            continue;
        }
        let extension = url
            .path()
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase());
        let numbered = numbered_document_link(&url);
        if numbered == Some(NumberedLink::Document) && url.origin() == page.url.origin() {
            let label = anchor.text().collect::<Vec<_>>().join(" ");
            document_pages.push((url, label));
        } else if numbered == Some(NumberedLink::Download)
            || anchor.value().attr("download").is_some()
            || extension.as_deref().is_some_and(|extension| {
                matches!(
                    extension,
                    "pdf"
                        | "zip"
                        | "doc"
                        | "docx"
                        | "odt"
                        | "xls"
                        | "xlsx"
                        | "ods"
                        | "ppt"
                        | "pptx"
                        | "csv"
                        | "rtf"
                        | "txt"
                        | "xml"
                )
            })
        {
            let label = anchor.text().collect::<Vec<_>>().join(" ");
            files.push((url, label));
        } else if url.origin() == page.url.origin()
            && anchor
                .text()
                .flat_map(|part| part.split(|ch: char| !ch.is_alphabetic()))
                .any(|word| {
                    matches!(
                        word.to_ascii_lowercase().as_str(),
                        "document"
                            | "documents"
                            | "attachment"
                            | "attachments"
                            | "form"
                            | "forms"
                            | "guidance"
                    )
                })
        {
            candidates
                .entry(url)
                .or_insert_with(|| anchor.text().collect::<Vec<_>>().join(" "));
        }
    }
    // Pages about one numbered document are read like file links, which follow HTML to the
    // file, unless the page already links the files themselves.
    if files.is_empty() {
        files = document_pages;
    }
    if let Some(scope) = scope {
        let terms = attachment_scope_terms(scope);
        let matches: Vec<_> = files
            .iter()
            .filter(|(url, title)| {
                let description = format!("{title} {}", url.path());
                let words = attachment_scope_terms(&description);
                terms.iter().any(|word| words.contains(word))
            })
            .cloned()
            .collect();
        if !matches.is_empty() {
            files = matches;
        }
    }
    let (pages, scope) = if files.is_empty() && candidates.len() == 1 {
        let (page, label) = candidates.into_iter().next().unwrap();
        (vec![page], Some(label))
    } else {
        (Vec::new(), None)
    };
    Attachments {
        pages,
        files: files.into_iter().map(|(url, _)| url).collect(),
        fallback: true,
        scope,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumberedLink {
    /// The link serves the file, as in `document_download_123.html` or `/download/123`.
    Download,
    /// The link opens a page about one document, as in `document_123.html`.
    Document,
}

/// Recognizes a link named after a numbered document, as E-ZAK and similar portals write
/// them. The last path segment, without a page extension such as `.html`, ends in a number,
/// and the word before the number says whether the link downloads the file or opens a page
/// about it. A bare number takes its word from the segment before, as in `/download/123`.
fn numbered_document_link(url: &Url) -> Option<NumberedLink> {
    let segments: Vec<&str> = url
        .path_segments()?
        .filter(|segment| !segment.is_empty())
        .collect();
    let (last, before) = segments.split_last()?;
    let stem = match last.rsplit_once('.') {
        Some((stem, extension))
            if matches!(
                extension.to_ascii_lowercase().as_str(),
                "html" | "htm" | "php" | "asp" | "aspx" | "jsp"
            ) =>
        {
            stem
        }
        Some(_) => return None,
        None => last,
    };
    let mut words: Vec<String> = stem
        .split(['_', '-'])
        .map(str::to_ascii_lowercase)
        .collect();
    if words.len() == 1 {
        words.insert(0, before.last()?.to_ascii_lowercase());
    }
    let (number, words) = words.split_last()?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    match words.last()?.as_str() {
        "download" => Some(NumberedLink::Download),
        "document" | "dokument" | "attachment" | "file" => Some(NumberedLink::Document),
        _ => None,
    }
}

fn attachment_scope_terms(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphabetic())
        .filter_map(|word| {
            let word = word.to_ascii_lowercase();
            (word.len() >= 5
                && !matches!(
                    word.as_str(),
                    "forms"
                        | "guidance"
                        | "document"
                        | "documents"
                        | "attachment"
                        | "attachments"
                        | "download"
                        | "related"
                        | "standard"
                        | "contract"
                        | "tender"
                        | "further"
                ))
            .then_some(word)
        })
        .collect()
}

fn resolve_one(node: &Node<'_>, select: Option<&Select>, base: &Url) -> Result<Option<Url>> {
    select
        .and_then(|rule| node.first(rule))
        .map(|url| resolve(base, &url))
        .transpose()
}

fn resolve_many(node: &Node<'_>, select: Option<&Select>, base: &Url) -> Result<Vec<Url>> {
    select
        .map(|rule| node.values(rule))
        .unwrap_or_default()
        .iter()
        .map(|url| resolve(base, url))
        .collect()
}

fn resolve(base: &Url, value: &str) -> Result<Url> {
    let mut url = base
        .join(value)
        .context("could not resolve a portal link")?;
    ensure!(
        public_http(&url),
        "portal link is not a credential-free HTTP(S) URL"
    );
    url.set_fragment(None);
    Ok(url)
}

fn resolve_field_url(fields: &mut Fields, base: &Url) {
    if let Some(urls) = fields.get_mut("url") {
        *urls = urls
            .iter()
            .filter_map(|url| resolve(base, url).ok().map(|url| url.to_string()))
            .collect();
    }
}

fn incomplete(err: anyhow::Error) -> ReadFailure {
    ReadFailure::Incomplete(err.to_string())
}

pub fn sample(body: &str, max_chars: usize) -> String {
    body.chars().take(max_chars).collect()
}

#[derive(Serialize)]
pub(crate) struct PageSample {
    pub body: String,
    pub links: Vec<SampleLink>,
}

#[derive(Serialize)]
pub(crate) struct SampleLink {
    label: String,
    pub url: String,
}

pub(crate) fn sample_page(page: &Page, limit: usize) -> PageSample {
    const LINK_LIMIT: usize = 40;
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    let mut add_link = |href: &str, label: String| {
        if links.len() < LINK_LIMIT
            && let Ok(url) = resolve(&page.url, href)
            && url != page.url
            && seen.insert(url.clone())
        {
            links.push(SampleLink {
                label: sample(&label, 120),
                url: url.to_string(),
            });
        }
    };

    let body = if let Ok(json) = serde_json::from_str::<Value>(&page.body) {
        let body = sample(&page.body, limit);
        for value in json_strings(&json) {
            if value.starts_with("http://") || value.starts_with("https://") {
                add_link(value, String::new());
            }
        }
        body
    } else {
        let mut document = Html::parse_document(&page.body);
        let noise = Selector::parse("script, style, svg").unwrap();
        let nodes: Vec<_> = document.select(&noise).map(|node| node.id()).collect();
        for id in nodes {
            if let Some(mut node) = document.tree.get_mut(id) {
                node.detach();
            }
        }
        let content = main_content(&document);
        let anchors = Selector::parse("a[href]").unwrap();
        for anchor in content.select(&anchors) {
            if let Some(href) = anchor
                .value()
                .attr("href")
                .filter(|href| !href.trim().is_empty())
            {
                let label = anchor
                    .text()
                    .flat_map(str::split_whitespace)
                    .collect::<Vec<_>>()
                    .join(" ");
                add_link(href, label);
            }
        }
        sample(&content.html(), limit)
    };
    PageSample { body, links }
}

fn main_content(document: &Html) -> ElementRef<'_> {
    ["main", "[role=main]", "body"]
        .iter()
        .find_map(|tag| document.select(&Selector::parse(tag).unwrap()).next())
        .unwrap_or_else(|| document.root_element())
}

fn json_strings(value: &Value) -> Vec<&str> {
    let mut pending = vec![value];
    let mut strings = Vec::new();
    while let Some(value) = pending.pop() {
        match value {
            Value::String(text) => strings.push(text.as_str()),
            Value::Array(items) => pending.extend(items.iter().rev()),
            Value::Object(items) => pending.extend(items.values().rev()),
            _ => {}
        }
    }
    strings
}

/// Words portals print where a value is missing. A missing value is published as missing.
const NOT_STATED: &[&str] = &[
    "-",
    "–",
    "—",
    "n/a",
    "n.v.t.",
    "none",
    "unknown",
    "not stated",
    "not specified",
    "not known",
    "niet vermeld",
    "niet bekend",
    "onbekend",
    "nicht angegeben",
    "keine angabe",
    "unbekannt",
    "non précisé",
    "non renseigné",
    "non communiqué",
    "inconnu",
    "no especificado",
    "no consta",
    "desconocido",
    "non specificato",
    "nie podano",
    "brak",
];

fn stated(value: String) -> Option<String> {
    let normalized = value.trim().trim_end_matches('.').to_lowercase();
    (!NOT_STATED.contains(&normalized.as_str())).then_some(value)
}

/// Maps a procedure name in the portal's language to the appendix values. "Not open" forms are
/// checked before "open", and Polish "nieograniczony" (unlimited) means open.
fn procedure(value: &str) -> Procedure {
    let value = value.to_lowercase();
    let has = |terms: &[&str]| terms.iter().any(|term| value.contains(term));
    if has(&["nieograniczony"]) {
        Procedure::Open
    } else if has(&[
        "niet-openbaar",
        "niet openbaar",
        "nichtoffen",
        "nicht offen",
        "nicht-offen",
        "restricted",
        "restreint",
        "restringido",
        "ristretta",
        "ograniczony",
    ]) {
        Procedure::Restricted
    } else if has(&[
        "negotiat",
        "verhandlung",
        "négoci",
        "negoci",
        "onderhandel",
        "negoziat",
        "negocjac",
    ]) {
        Procedure::Negotiated
    } else if has(&["open", "offen", "ouvert", "abierto", "aperta"]) {
        Procedure::Open
    } else {
        Procedure::Other
    }
}

fn normalize(fields: Fields, recipe: &Recipe) -> Tender {
    let get = |key: &str| {
        fields
            .get(key)
            .and_then(|values| values.first())
            .cloned()
            .and_then(stated)
    };
    let status = get("status").unwrap_or_default().to_lowercase();
    let status = match status.as_str() {
        "withdrawn" | "cancelled" | "canceled" | "annulé" | "annulée" | "aufgehoben"
        | "cancelado" | "cancelada" | "ingetrokken" => TenderStatus::Withdrawn,
        "closed" | "expired" | "fermé" | "fermée" | "geschlossen" | "gesloten" | "cerrado"
        | "cerrada" => TenderStatus::Closed,
        _ => TenderStatus::Open,
    };
    let place = Place {
        postal_code: get("postalCode"),
        city: get("city"),
        country_code: get("countryCode")
            .map(|code| code.to_ascii_uppercase())
            .filter(|code| code.len() == 2 && code.bytes().all(|ch| ch.is_ascii_alphabetic())),
    };
    let cpv = fields
        .get("cpvCodes")
        .map(|values| {
            let mut codes: Vec<_> = values
                .iter()
                .flat_map(|value| {
                    value.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';'))
                })
                .filter_map(|raw| {
                    let code = raw.split_once('-').map_or(raw, |(code, _)| code);
                    (code.len() == 8 && code.bytes().all(|ch| ch.is_ascii_digit()))
                        .then_some(code.to_owned())
                })
                .collect();
            codes.sort();
            codes.dedup();
            codes
        })
        .filter(|codes| !codes.is_empty());
    Tender {
        id: get("id"),
        url: get("url"),
        title: get("title"),
        buyer_name: get("buyerName"),
        publication_date: get("publicationDate").and_then(|value| {
            NaiveDate::parse_from_str(&value, "%Y-%m-%d")
                .ok()
                .or_else(|| {
                    recipe
                        .date_format
                        .as_deref()
                        .and_then(|format| NaiveDate::parse_from_str(&value, format).ok())
                })
                .or_else(|| {
                    DateTime::parse_from_rfc3339(&value)
                        .ok()
                        .map(|date| date.date_naive())
                })
        }),
        submission_deadline: get("submissionDeadline").and_then(|value| deadline(&value, recipe)),
        status,
        documents: Vec::new(),
        reference_number: get("referenceNumber"),
        description: get("description"),
        procedure_type: get("procedureType").map(|value| procedure(&value)),
        contract_nature: get("contractNature").and_then(|value| {
            match value.to_lowercase().as_str() {
                "works" | "travaux" | "bauleistungen" | "obras" | "werken" => Some(Nature::Works),
                "services" | "dienstleistungen" | "servicios" | "diensten" => {
                    Some(Nature::Services)
                }
                "supplies" | "fournitures" | "lieferleistungen" | "suministros" | "leveringen" => {
                    Some(Nature::Supplies)
                }
                _ => None,
            }
        }),
        cpv_codes: cpv,
        place_of_performance: (place.city.is_some()
            || place.postal_code.is_some()
            || place.country_code.is_some())
        .then_some(place),
    }
}

fn deadline(value: &str, recipe: &Recipe) -> Option<DateTime<FixedOffset>> {
    if let Ok(date) = DateTime::parse_from_rfc3339(value) {
        return Some(date);
    }
    let format = recipe
        .deadline_format
        .as_deref()
        .unwrap_or("%Y-%m-%d %H:%M");
    if let Ok(date) = DateTime::parse_from_str(value, format) {
        return Some(date);
    }
    let tz: Tz = recipe.timezone.as_ref()?.parse().ok()?;
    let local = NaiveDateTime::parse_from_str(value, format).ok()?;
    tz.from_local_datetime(&local)
        .single()
        .map(|date| date.fixed_offset())
}

#[cfg(test)]
mod recipe_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn items_accepts_a_string_or_a_selection_without_attribute() {
        for items in [
            json!(".gem-c-document-list__item"),
            json!({"path": ".gem-c-document-list__item"}),
            json!({"path": ".gem-c-document-list__item", "attribute": ""}),
        ] {
            let recipe: Recipe =
                serde_json::from_value(json!({"listing": "html", "items": items})).unwrap();
            assert_eq!(recipe.items, ".gem-c-document-list__item");
            recipe.validate().unwrap();
            assert_eq!(
                serde_json::to_value(&recipe).unwrap()["items"],
                ".gem-c-document-list__item"
            );
        }
        for items in [
            json!({"path": ".row", "attribute": "href"}),
            json!({"path": ".row", "extra": true}),
            json!(3),
        ] {
            let error =
                serde_json::from_value::<Recipe>(json!({"listing": "html", "items": items}))
                    .unwrap_err()
                    .to_string();
            assert!(error.contains("items"), "{error}");
        }
    }
}

#[cfg(test)]
mod reader_fix_tests {
    use super::*;
    use crate::fetch::Fetcher;
    use axum::{
        Router,
        extract::{RawQuery, State},
        http::{Uri, header},
        response::{Html as HtmlBody, IntoResponse},
        routing::get,
    };
    use std::sync::{Arc, Mutex};

    type Requests = Arc<Mutex<Vec<String>>>;

    async fn serve(router: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await });
        format!("http://{address}")
    }

    fn at(hour: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::Utc
            .with_ymd_and_hms(2026, 11, 20, hour, 0, 0)
            .unwrap()
    }

    #[test]
    fn ocds_window_asks_find_a_tender_for_recent_tender_stage_releases() {
        let entry = Url::parse(
            "https://www.find-tender.service.gov.uk/api/1.0/ocdsReleasePackages?stages=tender&limit=25&cursor=abc",
        )
        .unwrap();
        assert_eq!(
            ocds_window(&entry, at(9)).as_str(),
            "https://www.find-tender.service.gov.uk/api/1.0/ocdsReleasePackages?\
             updatedFrom=2026-08-22T09%3A00%3A00&updatedTo=2026-11-20T09%3A00%3A00\
             &stages=tender&limit=100"
        );
        let records = Url::parse("https://fts.example/api/1.0/ocdsRecordPackages?lang=en").unwrap();
        let windowed = ocds_window(&records, at(9));
        let query: Vec<_> = windowed
            .query_pairs()
            .map(|(key, _)| key.into_owned())
            .collect();
        assert_eq!(
            query,
            ["lang", "updatedFrom", "updatedTo", "stages", "limit"]
        );
    }

    #[test]
    fn ocds_window_uses_published_dates_for_contracts_finder_and_leaves_other_apis_alone() {
        let entry = Url::parse(
            "https://www.contractsfinder.service.gov.uk/Published/Notices/OCDS/Search?stages=award",
        )
        .unwrap();
        let windowed = ocds_window(&entry, at(0));
        let query: BTreeMap<_, _> = windowed.query_pairs().into_owned().collect();
        assert_eq!(query["publishedFrom"], "2026-08-22T00:00:00");
        assert_eq!(query["publishedTo"], "2026-11-20T00:00:00");
        assert_eq!(query["stages"], "tender");
        assert_eq!(query["limit"], "100");
        for other in [
            "https://portal.example/ocds?page=2",
            "https://portal.example/api/releases.json",
        ] {
            let url = Url::parse(other).unwrap();
            assert_eq!(ocds_window(&url, at(0)), url);
        }
    }

    #[tokio::test]
    async fn ocds_preset_reads_the_window_and_follows_the_cursor_to_the_last_page() {
        let requests = Requests::default();
        let router = Router::new()
            .route(
                "/api/1.0/ocdsReleasePackages",
                get(
                    |State(requests): State<Requests>, RawQuery(query): RawQuery| async move {
                        let query = query.unwrap_or_default();
                        requests.lock().unwrap().push(query.clone());
                        let body = if query.contains("cursor=") {
                            include_str!("../tests/fixtures/ocds/fts_window_page2.json")
                        } else {
                            include_str!("../tests/fixtures/ocds/fts_window_page1.json")
                        };
                        ([(header::CONTENT_TYPE, "application/json")], body)
                    },
                ),
            )
            .with_state(requests.clone());
        let base = serve(router).await;
        let candidate = ConnectionCandidate {
            method: ConnectionMethod::Ocds,
            entry_url: format!("{base}/api/1.0/ocdsReleasePackages?stages=tender&limit=25"),
            found_via: "test".into(),
            http_status: 200,
            content_type: Some("application/json".into()),
            sample_items: 2,
            fields_seen: Vec::new(),
            has_pagination: true,
            score: 1.0,
        };
        let recipe = Recipe::preset(&candidate).unwrap();
        let files = tempfile::tempdir().unwrap();
        let portal = Url::parse(&base).unwrap();
        let http = Fetcher::new(files.path().into()).unwrap().portal(&portal);
        let started = chrono::Utc::now();
        let read = read(&http, &portal, &recipe).await.unwrap();

        let ids: Vec<_> = read.tenders.iter().filter_map(|t| t.id.clone()).collect();
        assert_eq!(ids, ["041234-2026", "041235-2026", "041236-2026"]);
        assert_eq!(read.evidence.end_of_list, "last-page");
        assert_eq!(read.evidence.pages, 2);
        let requests = requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        let first: BTreeMap<String, String> = url_query(&requests[0]);
        assert_eq!(first["stages"], "tender");
        assert_eq!(first["limit"], "100");
        let parse = |value: &str| {
            NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S")
                .unwrap()
                .and_utc()
        };
        let (from, to) = (parse(&first["updatedFrom"]), parse(&first["updatedTo"]));
        assert_eq!(to - from, chrono::Duration::days(OCDS_WINDOW_DAYS));
        assert!(
            (to - started).num_seconds().abs() <= 60,
            "{to} vs {started}"
        );
        assert!(url_query(&requests[1]).contains_key("cursor"));
        assert!(read.evidence.listing_pages[0].contains("updatedFrom="));
    }

    fn url_query(query: &str) -> BTreeMap<String, String> {
        Url::parse(&format!("https://x.example/?{query}"))
            .unwrap()
            .query_pairs()
            .into_owned()
            .collect()
    }

    #[test]
    fn numbered_document_links_are_recognized_by_their_last_words() {
        for (link, expected) in [
            ("/document_download_101.html", Some(NumberedLink::Download)),
            ("/document_101.html", Some(NumberedLink::Document)),
            ("/document_101/", Some(NumberedLink::Document)),
            ("/files/download/77", Some(NumberedLink::Download)),
            ("/download-12.php", Some(NumberedLink::Download)),
            ("/contract_display_7.html", None),
            ("/tenders/101", None),
            ("/document_101.pdf", None),
            ("/document_download.html", None),
            ("/101", None),
        ] {
            let url = Url::parse("https://zakazky.example.cz")
                .unwrap()
                .join(link)
                .unwrap();
            assert_eq!(numbered_document_link(&url), expected, "{link}");
        }
    }

    #[test]
    fn html_fallback_prefers_download_links_over_document_pages() {
        let page = Page {
            url: Url::parse("https://zakazky.example.cz/contract_display_7.html").unwrap(),
            body: include_str!("../tests/fixtures/ezak/contract_display.html").into(),
        };
        let found = html_attachment_links(&page);
        let files: Vec<_> = found.files.iter().map(Url::as_str).collect();
        assert_eq!(
            files,
            [
                "https://zakazky.example.cz/document_101/",
                "https://zakazky.example.cz/document_102.html"
            ]
        );
        let page = Page {
            url: Url::parse("https://zakazky.example.cz/document_101/").unwrap(),
            body: include_str!("../tests/fixtures/ezak/document_101.html").into(),
        };
        let files: Vec<_> = html_attachment_links(&page)
            .files
            .iter()
            .map(Url::to_string)
            .collect();
        assert_eq!(
            files,
            ["https://zakazky.example.cz/document_download_101.html"]
        );
    }

    #[tokio::test]
    async fn ezak_document_pages_lead_to_files_named_by_content_disposition() {
        let requests = Requests::default();
        let page = |body: &'static str| HtmlBody(body).into_response();
        let router = Router::new()
            .route(
                "/document_101/",
                get(move || async move {
                    page(include_str!("../tests/fixtures/ezak/document_101.html"))
                }),
            )
            .route(
                "/document_102.html",
                get(move || async move {
                    page(include_str!("../tests/fixtures/ezak/document_102.html"))
                }),
            )
            .route(
                "/document_103.html",
                get(move || async move { page("<html><body><p>Smazáno</p></body></html>") }),
            )
            .route(
                "/document_download_101.html",
                get(|| async {
                    (
                        [
                            (header::CONTENT_TYPE, "application/pdf"),
                            (
                                header::CONTENT_DISPOSITION,
                                "attachment; filename=\"Zadavaci.pdf\"; \
                                 filename*=UTF-8''Zad%C3%A1vac%C3%AD%20dokumentace.pdf",
                            ),
                        ],
                        "%PDF-1.4 tender",
                    )
                }),
            )
            .route(
                "/document_download_102.html",
                get(|| async {
                    (
                        [
                            (header::CONTENT_TYPE, "application/octet-stream"),
                            (
                                header::CONTENT_DISPOSITION,
                                "attachment;filename=\"vykaz_vymer.xlsx\"",
                            ),
                        ],
                        "spreadsheet bytes",
                    )
                }),
            )
            .fallback(
                |uri: Uri| async move { (axum::http::StatusCode::NOT_FOUND, uri.to_string()) },
            )
            .layer(axum::middleware::from_fn_with_state(
                requests.clone(),
                |State(requests): State<Requests>,
                 request: axum::extract::Request,
                 next: axum::middleware::Next| async move {
                    requests
                        .lock()
                        .unwrap()
                        .push(request.uri().path().to_owned());
                    next.run(request).await
                },
            ));
        let base = serve(router).await;
        let detail = Page {
            url: Url::parse(&format!("{base}/contract_display_7.html")).unwrap(),
            body: include_str!("../tests/fixtures/ezak/contract_display.html").into(),
        };
        let recipe: Recipe =
            serde_json::from_value(serde_json::json!({"listing": "html", "items": "tr"})).unwrap();
        let files = tempfile::tempdir().unwrap();
        let http = Fetcher::new(files.path().into())
            .unwrap()
            .portal(&detail.url);
        let mut samples = Vec::new();
        let mut documents = attachment_files(
            &http,
            html_attachment_links(&detail),
            &recipe,
            Some(detail.url.clone()),
            &mut samples,
        )
        .await
        .unwrap();
        documents.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        let names: Vec<_> = documents.iter().map(|d| d.file_name.as_str()).collect();
        assert_eq!(names, ["Zadávací dokumentace.pdf", "vykaz_vymer.xlsx"]);
        assert_eq!(documents[0].size_bytes, "%PDF-1.4 tender".len() as u64);
        assert_eq!(
            requests.lock().unwrap().clone(),
            [
                "/document_101/",
                "/document_102.html",
                "/document_download_101.html",
                "/document_download_102.html"
            ]
        );

        // A document page that links no file is skipped, not a broken read.
        let empty = Attachments {
            files: vec![Url::parse(&format!("{base}/document_103.html")).unwrap()],
            fallback: true,
            ..Attachments::default()
        };
        let documents = attachment_files(&http, empty, &recipe, None, &mut samples)
            .await
            .unwrap();
        assert!(documents.is_empty());
    }
}

#[cfg(test)]
mod sample_tests {
    use super::*;

    #[test]
    fn samples_tenders_behind_more_than_twenty_thousand_characters() {
        let page = Page {
            url: Url::parse("https://portal.example/list/current").unwrap(),
            body: format!(
                r#"<body><header>{}</header><main id="tenders"><script>{}</script>
                <style>.invisible{{color:red}}</style><svg><title>Ignore icon</title></svg>
                <h1>Open tenders</h1><table class="opportunities"><tr data-tender="T1"><td>
                <a href="../documents/spec.pdf#download">Tender specification</a>
                <a href="../documents/spec.pdf">Duplicate</a>
                <a href="//files.example/terms.pdf">Terms</a></td></tr></table></main></body>"#,
                "outside-main ".repeat(2500),
                "inline-script ".repeat(2500)
            ),
        };
        let evidence = sample_page(&page, 20_000);
        assert!(evidence.body.contains("Open tenders"));
        assert!(evidence.body.contains("id=\"tenders\""));
        assert!(evidence.body.contains("class=\"opportunities\""));
        assert!(evidence.body.contains("data-tender=\"T1\""));
        for excluded in ["outside-main", "inline-script", "<style", "<svg"] {
            assert!(!evidence.body.contains(excluded));
        }
        assert_eq!(evidence.links.len(), 2);
        assert_eq!(evidence.links[0].label, "Tender specification");
        assert_eq!(
            evidence.links[0].url,
            "https://portal.example/documents/spec.pdf"
        );
        assert_eq!(evidence.links[1].url, "https://files.example/terms.pdf");
    }

    #[test]
    fn prefers_main_landmark_and_falls_back_to_body_with_a_link_limit() {
        let url = Url::parse("https://portal.example/").unwrap();
        let landmark = Page {
            url: url.clone(),
            body: r#"<body><nav>Menu</nav><section role="main">Tenders</section></body>"#.into(),
        };
        assert!(!sample_page(&landmark, 100).body.contains("Menu"));
        let links = (1..=45)
            .map(|i| format!("<a href=\"/file/{i}\">File {i}</a>"))
            .collect::<String>();
        let body = Page {
            url,
            body: format!("<body><h2>Opportunities</h2>{links}</body>"),
        };
        let evidence = sample_page(&body, 45);
        assert!(evidence.body.contains("Opportunities"));
        assert!(evidence.body.chars().count() <= 45);
        assert_eq!(evidence.links.len(), 40);
        assert_eq!(evidence.links[39].url, "https://portal.example/file/40");
    }

    #[test]
    fn preserves_raw_public_json_and_observed_absolute_links() {
        let page = Page {
            url: Url::parse("https://portal.example/data.json").unwrap(),
            body: r#"{"tenders":[{"title":"Road works","url":"https://files.example/spec.pdf"}]}"#
                .into(),
        };
        let evidence = sample_page(&page, 20_000);
        assert_eq!(evidence.body, page.body);
        assert_eq!(evidence.links[0].url, "https://files.example/spec.pdf");
        assert_eq!(sample_page(&page, 5).body, "{\"ten");
    }

    #[test]
    fn totals_parse_a_clear_leading_count_only() {
        assert_eq!(parse_total("57"), Some(57));
        assert_eq!(parse_total(" 1.234 "), Some(1234));
        assert_eq!(parse_total("1,234,567"), Some(1_234_567));
        assert_eq!(parse_total("1\u{a0}234"), Some(1234));
        assert_eq!(
            parse_total("8 aanbestedingen gevonden. Pagina 1 van 2."),
            Some(8)
        );
        assert_eq!(parse_total("1.234 results"), Some(1234));
        assert_eq!(parse_total("1 234 Ausschreibungen"), Some(1234));
        assert_eq!(parse_total("57 results"), Some(57));
        for text in [
            "Page 1 of 2",
            "1–20 of 57",
            "1 of 2",
            "Pagina 1 van 2",
            "12.5",
            "1.23",
            "57results",
            "",
        ] {
            assert_eq!(parse_total(text), None, "{text:?}");
        }
    }

    #[test]
    fn listing_links_mark_pagination_but_not_page_layout_classes() {
        let base = Url::parse("https://portal.example/list").unwrap();
        let html = r#"<div class="page-wrapper"><main>
            <a href="/tender/1">Tender</a>
            <a href="?page=2" rel="next">2</a>
            <a class="pager-last" href="?page=9">Last</a>
            <nav aria-label="Pagination"><ul><li><a href="?page=3">3</a></li></ul></nav>
            <a href="https://elsewhere.example/?page=2" class="next">Other site</a>
            <a href="/help#top">Help</a></main></div>"#;
        let links = listing_links(&Source::Html(Html::parse_document(html)), &base);
        let mut pagination: Vec<_> = links.pagination.iter().map(Url::as_str).collect();
        pagination.sort_unstable();
        assert_eq!(
            pagination,
            [
                "https://portal.example/list?page=2",
                "https://portal.example/list?page=3",
                "https://portal.example/list?page=9",
            ]
        );
        assert!(links.all.contains("https://portal.example/help"));
        assert!(links.all.contains("https://portal.example/tender/1"));
        assert!(!links.all.iter().any(|link| link.contains("elsewhere")));
    }

    #[test]
    fn procedures_map_by_keyword_and_placeholders_count_as_missing() {
        for (text, expected) in [
            ("Openbaar", Procedure::Open),
            ("Niet-openbaar", Procedure::Restricted),
            ("Offenes Verfahren", Procedure::Open),
            ("Nicht offenes Verfahren", Procedure::Restricted),
            ("Verhandlungsverfahren", Procedure::Negotiated),
            ("Procédure ouverte", Procedure::Open),
            (
                "Procédure concurrentielle avec négociation",
                Procedure::Negotiated,
            ),
            ("Przetarg nieograniczony", Procedure::Open),
            ("Przetarg ograniczony", Procedure::Restricted),
            ("Dynamic purchasing system", Procedure::Other),
        ] {
            assert_eq!(procedure(text), expected, "{text}");
        }
        assert_eq!(stated("Niet vermeld".into()), None);
        assert_eq!(stated(" Not specified. ".into()), None);
        assert_eq!(stated("—".into()), None);
        assert_eq!(
            stated("Stichting Openbaar Onderwijs".into()).as_deref(),
            Some("Stichting Openbaar Onderwijs")
        );
    }
}
