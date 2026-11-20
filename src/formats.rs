//! Parsers for listing formats with a fixed structure: RSS and Atom feeds, XML sitemaps, and
//! OCDS release or record packages. The recipe reader turns their output into listing rows.

use anyhow::{Result, bail};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// One feed entry: its title and its link to the notice.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct FeedEntry {
    pub title: Option<String>,
    pub link: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Feed {
    pub entries: Vec<FeedEntry>,
    /// An Atom `link rel="next"` on the feed itself.
    pub next: Option<String>,
}

/// Reads RSS 2.0, RSS 1.0 (RDF) and Atom. Entries are `item` or `entry` elements.
pub(crate) fn feed(body: &str) -> Result<Feed> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut feed = Feed::default();
    let mut root: Option<String> = None;
    let mut path: Vec<String> = Vec::new();
    let mut entry: Option<FeedEntry> = None;
    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                let name = local(&element);
                root.get_or_insert_with(|| name.clone());
                if matches!(name.as_str(), "item" | "entry") {
                    entry = Some(FeedEntry::default());
                }
                feed_link(&element, &name, entry.as_mut(), &mut feed.next);
                path.push(name);
            }
            Event::Empty(element) => {
                let name = local(&element);
                feed_link(&element, &name, entry.as_mut(), &mut feed.next);
            }
            Event::Text(text) => {
                let text = text.unescape()?.trim().to_owned();
                feed_text(&path, entry.as_mut(), text);
            }
            Event::CData(text) => {
                let text = String::from_utf8_lossy(&text.into_inner())
                    .trim()
                    .to_owned();
                feed_text(&path, entry.as_mut(), text);
            }
            Event::End(_) => {
                if let Some(name) = path.pop()
                    && matches!(name.as_str(), "item" | "entry")
                    && let Some(done) = entry.take()
                {
                    feed.entries.push(done);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    match root.as_deref() {
        Some("rss" | "feed" | "RDF") => Ok(feed),
        _ => bail!("expected an RSS or Atom feed"),
    }
}

fn feed_text(path: &[String], entry: Option<&mut FeedEntry>, text: String) {
    let (Some(entry), Some(name)) = (entry, path.last()) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    match name.as_str() {
        "title" if entry.title.is_none() => entry.title = Some(text),
        // RSS gives the link as text; Atom uses an href attribute handled in `feed_link`.
        "link" if entry.link.is_none() => entry.link = Some(text),
        _ => {}
    }
}

fn feed_link(
    element: &BytesStart<'_>,
    name: &str,
    entry: Option<&mut FeedEntry>,
    next: &mut Option<String>,
) {
    if name != "link" {
        return;
    }
    let Some(href) = attribute(element, "href") else {
        return;
    };
    let rel = attribute(element, "rel").unwrap_or_else(|| "alternate".into());
    match entry {
        Some(entry) if rel == "alternate" && entry.link.is_none() => entry.link = Some(href),
        None if rel == "next" => *next = Some(href),
        _ => {}
    }
}

#[derive(Debug, Default)]
pub(crate) struct Sitemap {
    /// Page URLs from a `urlset`.
    pub urls: Vec<String>,
    /// Child sitemaps from a `sitemapindex`.
    pub sitemaps: Vec<String>,
}

/// Reads a sitemap `urlset` or a `sitemapindex`.
pub(crate) fn sitemap(body: &str) -> Result<Sitemap> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut map = Sitemap::default();
    let mut root: Option<String> = None;
    let mut path: Vec<String> = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(element) => {
                let name = local(&element);
                root.get_or_insert_with(|| name.clone());
                path.push(name);
            }
            Event::Text(text) => {
                let text = text.unescape()?.trim().to_owned();
                let parent = path.len().checked_sub(2).and_then(|index| path.get(index));
                if path.last().map(String::as_str) == Some("loc") && !text.is_empty() {
                    match parent.map(String::as_str) {
                        Some("url") => map.urls.push(text),
                        Some("sitemap") => map.sitemaps.push(text),
                        _ => {}
                    }
                }
            }
            Event::End(_) => {
                path.pop();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    match root.as_deref() {
        Some("urlset" | "sitemapindex") => Ok(map),
        _ => bail!("expected an XML sitemap"),
    }
}

fn local(element: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(element.local_name().as_ref()).into_owned()
}

fn attribute(element: &BytesStart<'_>, name: &str) -> Option<String> {
    element
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.local_name().as_ref() == name.as_bytes())
        .and_then(|attribute| attribute.unescape_value().ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// One tender from an OCDS package, with values in the reader's field vocabulary.
#[derive(Debug, Default)]
pub(crate) struct OcdsTender {
    pub fields: BTreeMap<String, Vec<String>>,
    pub documents: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct OcdsPage {
    pub tenders: Vec<OcdsTender>,
    pub next: Option<String>,
}

/// Reads an OCDS release package (`releases`) or record package (`records`). A record uses its
/// compiled release, or else its latest release. Several releases of one `ocid` keep the latest.
pub(crate) fn ocds(package: &Value) -> Result<OcdsPage> {
    let releases: Vec<&Value> = if let Some(releases) = package["releases"].as_array() {
        releases.iter().collect()
    } else if let Some(records) = package["records"].as_array() {
        records
            .iter()
            .filter_map(|record| {
                let compiled = &record["compiledRelease"];
                if compiled.is_object() {
                    Some(compiled)
                } else {
                    latest(record["releases"].as_array()?.iter())
                }
            })
            .collect()
    } else {
        bail!("expected an OCDS release or record package");
    };
    let mut by_ocid: HashMap<String, &Value> = HashMap::new();
    let mut order = Vec::new();
    for release in releases {
        let key = text(&release["ocid"])
            .or_else(|| text(&release["tender"]["id"]))
            .unwrap_or_else(|| order.len().to_string());
        match by_ocid.get(&key) {
            Some(existing) if date(existing) >= date(release) => {}
            Some(_) => {
                by_ocid.insert(key, release);
            }
            None => {
                order.push(key.clone());
                by_ocid.insert(key, release);
            }
        }
    }
    Ok(OcdsPage {
        tenders: order.iter().map(|key| ocds_tender(by_ocid[key])).collect(),
        next: text(&package["links"]["next"]),
    })
}

fn latest<'a>(releases: impl Iterator<Item = &'a Value>) -> Option<&'a Value> {
    releases.max_by(|left, right| date(left).cmp(&date(right)))
}

fn date(release: &Value) -> String {
    text(&release["date"]).unwrap_or_default()
}

fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn ocds_tender(release: &Value) -> OcdsTender {
    let tender = &release["tender"];
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut set = |name: &str, value: Option<String>| {
        if let Some(value) = value {
            fields.insert(name.into(), vec![value]);
        }
    };
    set("id", text(&tender["id"]).or_else(|| text(&release["ocid"])));
    set(
        "url",
        text(&release["url"]).or_else(|| text(&tender["url"])),
    );
    set("title", text(&tender["title"]));
    set("description", text(&tender["description"]));
    set(
        "buyerName",
        text(&release["buyer"]["name"]).or_else(|| {
            release["parties"].as_array()?.iter().find_map(|party| {
                let buyer = party["roles"]
                    .as_array()?
                    .iter()
                    .any(|role| role.as_str() == Some("buyer"));
                buyer.then(|| text(&party["name"])).flatten()
            })
        }),
    );
    set("publicationDate", text(&release["date"]));
    set(
        "submissionDeadline",
        text(&tender["tenderPeriod"]["endDate"]),
    );
    set(
        "status",
        text(&tender["status"]).map(|status| {
            match status.as_str() {
                "cancelled" | "withdrawn" => "withdrawn",
                "complete" | "unsuccessful" => "closed",
                _ => "open",
            }
            .to_owned()
        }),
    );
    set(
        "procedureType",
        text(&tender["procurementMethod"]).map(|method| {
            match method.as_str() {
                "open" => "open",
                "selective" => "restricted",
                "limited" => "negotiated",
                _ => "other",
            }
            .to_owned()
        }),
    );
    set(
        "contractNature",
        text(&tender["mainProcurementCategory"]).and_then(|category| {
            match category.as_str() {
                "goods" => Some("supplies"),
                "works" => Some("works"),
                "services" => Some("services"),
                _ => None,
            }
            .map(str::to_owned)
        }),
    );
    let cpv: Vec<String> = tender["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| &item["classification"])
        .filter(|class| {
            text(&class["scheme"]).is_some_and(|scheme| scheme.eq_ignore_ascii_case("CPV"))
        })
        .filter_map(|class| text(&class["id"]))
        .collect();
    if !cpv.is_empty() {
        fields.insert("cpvCodes".into(), cpv);
    }
    let documents = tender["documents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|document| text(&document["url"]))
        .collect();
    OcdsTender { fields, documents }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_rss_atom_and_rdf_entries() {
        let rss = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Portal</title>
            <link>https://portal.example/</link>
            <item><title><![CDATA[Road & bridge]]></title><link>https://portal.example/t/1</link></item>
            <item><title>Park</title><link> https://portal.example/t/2 </link></item>
            </channel></rss>"#;
        let parsed = feed(rss).unwrap();
        assert_eq!(parsed.entries.len(), 2);
        assert_eq!(parsed.entries[0].title.as_deref(), Some("Road & bridge"));
        assert_eq!(
            parsed.entries[1].link.as_deref(),
            Some("https://portal.example/t/2")
        );

        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom">
            <link rel="self" href="https://portal.example/feed"/>
            <link rel="next" href="https://portal.example/feed?page=2"/>
            <entry><title>Survey</title><link rel="edit" href="https://portal.example/edit/1"/>
              <link href="https://portal.example/t/1"/></entry></feed>"#;
        let parsed = feed(atom).unwrap();
        assert_eq!(
            parsed.next.as_deref(),
            Some("https://portal.example/feed?page=2")
        );
        assert_eq!(
            parsed.entries[0].link.as_deref(),
            Some("https://portal.example/t/1")
        );

        assert!(feed("<html><body>Not a feed</body></html>").is_err());
    }

    #[test]
    fn reads_sitemap_urls_and_indexes() {
        let urls = r#"<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
            <url><loc>https://portal.example/t/1</loc><lastmod>2026-11-01</lastmod></url>
            <url><loc>https://portal.example/about</loc></url></urlset>"#;
        assert_eq!(sitemap(urls).unwrap().urls.len(), 2);
        let index = r#"<sitemapindex><sitemap><loc>https://portal.example/tenders.xml.gz</loc></sitemap></sitemapindex>"#;
        assert_eq!(
            sitemap(index).unwrap().sitemaps,
            ["https://portal.example/tenders.xml.gz"]
        );
        assert!(sitemap("<rss></rss>").is_err());
    }

    #[test]
    fn maps_ocds_releases_and_keeps_the_latest_version() {
        let package = json!({
            "links": {"next": "https://portal.example/ocds?page=2"},
            "releases": [
                {"ocid": "ocds-1", "date": "2026-11-01T09:00:00Z", "tender": {"id": "T-1", "title": "Old title"}},
                {"ocid": "ocds-1", "date": "2026-11-02T09:00:00+01:00",
                 "buyer": {"name": "City of Alpha"},
                 "tender": {"id": "T-1", "title": "Road survey", "status": "active",
                    "procurementMethod": "selective", "mainProcurementCategory": "goods",
                    "tenderPeriod": {"endDate": "2026-12-09T10:00:00+01:00"},
                    "items": [{"classification": {"scheme": "CPV", "id": "45000000-7"}},
                              {"classification": {"scheme": "UNSPSC", "id": "12345678"}}],
                    "documents": [{"url": "https://portal.example/doc.pdf"}]}},
                {"ocid": "ocds-2", "date": "2026-11-03T00:00:00Z",
                 "parties": [{"name": "Supplier", "roles": ["tenderer"]}, {"name": "City of Beta", "roles": ["buyer", "procuringEntity"]}],
                 "tender": {"title": "Park", "status": "cancelled", "procurementMethod": "direct"}}
            ]
        });
        let page = ocds(&package).unwrap();
        assert_eq!(
            page.next.as_deref(),
            Some("https://portal.example/ocds?page=2")
        );
        assert_eq!(page.tenders.len(), 2);
        let first = &page.tenders[0].fields;
        let get = |fields: &BTreeMap<String, Vec<String>>, key: &str| {
            fields.get(key).map(|v| v.join(","))
        };
        assert_eq!(get(first, "title").as_deref(), Some("Road survey"));
        assert_eq!(get(first, "id").as_deref(), Some("T-1"));
        assert_eq!(get(first, "buyerName").as_deref(), Some("City of Alpha"));
        assert_eq!(get(first, "procedureType").as_deref(), Some("restricted"));
        assert_eq!(get(first, "contractNature").as_deref(), Some("supplies"));
        assert_eq!(get(first, "cpvCodes").as_deref(), Some("45000000-7"));
        assert_eq!(
            get(first, "submissionDeadline").as_deref(),
            Some("2026-12-09T10:00:00+01:00")
        );
        assert_eq!(
            page.tenders[0].documents,
            ["https://portal.example/doc.pdf"]
        );
        let second = &page.tenders[1].fields;
        assert_eq!(get(second, "id").as_deref(), Some("ocds-2"));
        assert_eq!(get(second, "buyerName").as_deref(), Some("City of Beta"));
        assert_eq!(get(second, "status").as_deref(), Some("withdrawn"));
        assert_eq!(get(second, "procedureType").as_deref(), Some("other"));

        let records = json!({"records": [{"ocid": "ocds-3", "releases": [
            {"ocid": "ocds-3", "date": "2026-01-01T00:00:00Z", "tender": {"title": "First"}},
            {"ocid": "ocds-3", "date": "2026-02-01T00:00:00Z", "tender": {"title": "Second"}}]}]});
        let page = ocds(&records).unwrap();
        assert_eq!(page.tenders[0].fields["title"], ["Second"]);
        assert!(ocds(&json!({"items": []})).is_err());
    }
}
