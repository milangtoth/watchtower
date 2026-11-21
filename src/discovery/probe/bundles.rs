//! Discover public listings advertised by a thin JavaScript application shell.
use super::{NOTICE_WORDS, resolve};
use reqwest::Url;
use scraper::{ElementRef, Html, Selector};
use std::{
    cmp::Reverse,
    collections::{BTreeSet, HashSet},
};

pub(super) const MAX_BUNDLES: usize = 5;
pub(super) const MAX_ENDPOINTS: usize = 3;
pub(super) const SAMPLE_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub(super) struct Shell {
    pub(super) page: Url,
    pub(super) base: Url,
    pub(super) scripts: Vec<Url>,
}

/// Return None for ordinary pages. An empty vector denotes a shell with no eligible scripts.
pub(super) fn scripts(document: &Html, page: &Url) -> Option<Shell> {
    let select = |css| Selector::parse(css).expect("static CSS");
    let body = document.select(&select("body")).next()?;
    let root = body.descendants().filter_map(ElementRef::wrap).any(|el| {
        el.value().name().ends_with("-root")
            || el.value().attr("ng-version").is_some()
            || matches!(el.value().id(), Some("app" | "root" | "__next" | "__nuxt"))
    });
    if !root || document.select(&select("script[src]")).next().is_none() {
        return None;
    }
    let visible = body
        .descendants()
        .filter_map(|node| {
            let text = node.value().as_text()?;
            let hidden = node.ancestors().filter_map(ElementRef::wrap).any(|el| {
                matches!(
                    el.value().name(),
                    "script" | "style" | "noscript" | "template" | "svg"
                )
            });
            (!hidden).then_some(text)
        })
        .flat_map(|t| t.split_whitespace())
        .map(str::len);
    let mut visible_bytes = 0;
    for bytes in visible {
        visible_bytes += bytes;
        if visible_bytes > 320 {
            return None;
        }
    }
    let base = document
        .select(&select("base[href]"))
        .next()
        .and_then(|e| resolve(page, e.attr("href")?))
        .unwrap_or_else(|| page.clone());
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    for el in document.select(&select("script[src], link[rel=modulepreload][href]")) {
        let Some(url) = el
            .attr("src")
            .or_else(|| el.attr("href"))
            .and_then(|s| resolve(&base, s))
        else {
            continue;
        };
        if url.origin() == page.origin() && seen.insert(url.clone()) {
            urls.push(url);
        }
    }
    urls.sort_by_key(|u| {
        let name = u
            .path_segments()
            .and_then(|mut s| s.next_back())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.starts_with("main") || name.starts_with("app") {
            0
        } else if ["vendor", "polyfill", "runtime", "analytics"]
            .iter()
            .any(|s| name.contains(s))
        {
            2
        } else {
            1
        }
    });
    urls.truncate(MAX_BUNDLES);
    Some(Shell {
        page: page.clone(),
        base,
        scripts: urls,
    })
}

fn api_origin(url: &Url, page: &Url) -> bool {
    if url.origin() == page.origin() {
        return true;
    }
    let Some(host) = page.domain() else {
        return false;
    };
    let site = host.strip_prefix("www.").unwrap_or(host);
    url.scheme() == page.scheme()
        && url.port_or_known_default() == page.port_or_known_default()
        && url.domain().and_then(|h| h.strip_prefix("api.")) == Some(site)
}

// Resolve only the first page of a query template; never guess a dynamic resource ID or execute JS.
fn first_page(url: &mut Url) -> Option<()> {
    let mut pairs = Vec::new();
    for (key, value) in url.query_pairs() {
        let name = key.to_ascii_lowercase();
        if [
            "token",
            "key",
            "secret",
            "password",
            "signature",
            "authorization",
        ]
        .iter()
        .any(|s| name.contains(s))
        {
            return None;
        }
        if name == "query" && value.to_ascii_lowercase().contains("mutation") {
            return None;
        }
        let graphql = name == "query" && url.path().ends_with("/graphql") && !value.contains("${");
        let template = !graphql
            && (value.starts_with('{') || value.starts_with("${") || value.starts_with(':'));
        let initial = match name.as_str() {
            "page" | "pagenumber" => Some("1"),
            "pageindex" | "offset" | "start" => Some("0"),
            "limit" | "size" | "pagesize" | "per_page" => Some("5"),
            "q" | "search" | "term" | "cursor" => Some(""),
            _ => None,
        };
        let value = if template {
            initial?.to_owned()
        } else if value.is_empty() {
            initial.unwrap_or("").to_owned()
        } else {
            value.into_owned()
        };
        pairs.push((key.into_owned(), value));
    }
    if !pairs.is_empty() {
        url.query_pairs_mut().clear().extend_pairs(pairs);
    }
    Some(())
}

fn endpoint(literal: &str, base: &Url, page: &Url) -> Option<(Url, u8)> {
    if literal.len() > 2048 {
        return None;
    }
    let raw = literal.replace("\\/", "/");
    let path = raw.split('?').next()?;
    if path.is_empty()
        || path.contains(['{', '}', '\\', '#'])
        || path.chars().any(char::is_whitespace)
    {
        return None;
    }
    let mut url = resolve(base, &raw)?;
    if !api_origin(&url, page) {
        return None;
    }
    let lower = url.path().to_ascii_lowercase();
    if !(lower.contains("/api/")
        || lower.contains("/rest/")
        || lower.contains("/search")
        || lower.ends_with(".json")
        || lower.ends_with("/graphql"))
    {
        return None;
    }
    if [
        "manifest",
        "package.json",
        "tsconfig",
        "/assets/",
        "/i18n/",
        "/locales/",
        "/auth/",
        "/admin/",
        "/private/",
        "/logout",
        "/delete",
        "/update",
        "/create",
    ]
    .iter()
    .any(|s| lower.contains(s))
    {
        return None;
    }
    first_page(&mut url)?;
    let notice = NOTICE_WORDS.iter().any(|s| lower.contains(s));
    let search = lower.contains("search") || lower.contains("list");
    Some((url, 4 * u8::from(notice) + 2 * u8::from(search)))
}

/// Substitute `${Name}` placeholders bound to short string constants (`Name="search"`) and
/// report whether the result is a search or listing path. Unresolved placeholders never match.
fn post_search_path(literal: &str, js: &str) -> bool {
    let mut path = String::new();
    let mut rest = literal;
    while let Some(open) = rest.find("${") {
        path.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            return false;
        };
        let name = &rest[open + 2..open + close];
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        {
            return false;
        }
        let Some(value) = constant(js, name) else {
            return false;
        };
        path.push_str(value);
        rest = &rest[open + close + 1..];
    }
    path.push_str(rest);
    let lower = path.split('?').next().unwrap_or("").to_ascii_lowercase();
    lower.starts_with('/')
        && (lower.contains("search") || NOTICE_WORDS.iter().any(|s| lower.contains(s)))
}

/// Find `.post(<literal>` call sites directly. The literal scanner below can lose its place
/// in minified bundles (quotes inside templates and regexes), which hid Doffin's search.
fn post_calls(js: &str, base: &Url, page: &Url) -> bool {
    js.match_indices(".post(").any(|(at, call)| {
        let rest = js[at + call.len()..].trim_start();
        let Some(quote) = rest
            .chars()
            .next()
            .filter(|c| matches!(c, '\'' | '"' | '`'))
        else {
            return false;
        };
        let Some(end) = rest[1..].find([quote, '\n']) else {
            return false;
        };
        let literal = &rest[1..=end];
        literal.len() <= 2048
            && rest[1 + end..].starts_with(quote)
            && (endpoint(literal, base, page).is_some() || post_search_path(literal, js))
    })
}

fn constant<'a>(js: &'a str, name: &str) -> Option<&'a str> {
    let pattern = format!("{name}=");
    js.match_indices(&pattern).find_map(|(at, _)| {
        let prior = js[..at].chars().next_back()?;
        if !matches!(prior, ',' | ';' | ' ' | '\n' | '(' | '{') {
            return None;
        }
        let tail = &js[at + pattern.len()..];
        let quote = tail
            .chars()
            .next()
            .filter(|c| matches!(c, '\'' | '"' | '`'))?;
        let end = tail[1..].find(quote)?;
        let value = &tail[1..=end];
        (value.len() <= 64
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.')))
        .then_some(value)
    })
}

#[derive(Default)]
pub(super) struct Routes {
    get: BTreeSet<(Reverse<u8>, Url)>,
    pub(super) post_search: bool,
}

impl Routes {
    /// Read literal URL strings only. A bounded window around the literal distinguishes
    /// explicit POST searches from GET requests without evaluating the site's program.
    pub(super) fn inspect(&mut self, js: &str, base: &Url, page: &Url) {
        self.post_search |= post_calls(js, base, page);
        let bytes = js.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i..].starts_with(b"//") {
                i += js[i..].find('\n').unwrap_or(bytes.len() - i);
                continue;
            }
            if bytes[i..].starts_with(b"/*") {
                i = js[i + 2..].find("*/").map_or(bytes.len(), |n| i + 4 + n);
                continue;
            }
            let quote = bytes[i];
            if !matches!(quote, b'\'' | b'"' | b'\x60') {
                i += 1;
                continue;
            }
            let start = i;
            i += 1;
            while i < bytes.len() && bytes[i] != quote {
                if bytes[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            let literal = &js[start + 1..i];
            i += 1;
            let endpoint = endpoint(literal, base, page);
            if endpoint.is_none() && !(literal.starts_with('/') || literal.starts_with("${")) {
                continue;
            }
            let before = &js[js.ceil_char_boundary(start.saturating_sub(48))..start];
            let after = &js[i..js.floor_char_boundary((i + 180).min(js.len()))];
            let before = before
                .trim_end_matches(char::is_whitespace)
                .trim_end_matches('(')
                .trim_end();
            let options = after
                .trim_start()
                .strip_prefix(',')
                .map(str::trim_start)
                .filter(|s| s.starts_with('{'))
                .unwrap_or("")
                .split(['}', ')', ';'])
                .next()
                .unwrap_or("");
            let options: String = options
                .chars()
                .filter(|c| !c.is_whitespace() && !matches!(c, '\'' | '"'))
                .flat_map(char::to_lowercase)
                .collect();
            if before.ends_with(".post") || options.contains("method:post") {
                // A templated path such as `/${route}` counts when each placeholder names a
                // string constant in the same bundle and the resolved path is a search.
                self.post_search |= endpoint.is_some() || post_search_path(literal, js);
                continue;
            }
            let Some((url, score)) = endpoint else {
                continue;
            };
            if [".delete", ".put", ".patch"]
                .iter()
                .any(|m| before.ends_with(m))
                || (options.contains("method:") && !options.contains("method:get"))
            {
                continue;
            }
            self.get.insert((Reverse(score), url));
            if self.get.len() > 64 {
                self.get.pop_last();
            }
        }
    }

    pub(super) fn best(self) -> impl Iterator<Item = Url> {
        self.get.into_iter().map(|(_, url)| url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_literal_public_listing_routes_on_allowed_origins_are_sampled() {
        let page = Url::parse("https://www.portal.example/").unwrap();
        let mut routes = Routes::default();
        routes.inspect(
            r#"
            client.get("https://api.portal.example/rest/notices");
            fetch("/graphql?query=%7Bnotices%7Bid%7D%7D");
            fetch("https://api.portal.example.evil.example/api/notices");
            fetch("http://api.portal.example/api/notices");
            fetch("https://api.portal.example:8443/api/notices");
            fetch("/graphql?query=mutation%20Example");
            fetch("/api/notices?token=secret");
            fetch("/api/{id}/detail");
            fetch("/api/search", {method:"POST"});
            fetch("/api/write",{method:"PUT"});
            /* fetch("/api/commented") */
        "#,
            &page,
            &page,
        );
        assert!(routes.post_search);
        let endpoints: Vec<_> = routes.best().map(|u| u.to_string()).collect();
        assert_eq!(
            endpoints,
            [
                "https://api.portal.example/rest/notices",
                "https://www.portal.example/graphql?query=%7Bnotices%7Bid%7D%7D"
            ]
        );
    }

    #[test]
    fn templated_posts_count_only_when_they_resolve_to_a_search_path() {
        let page = Url::parse("https://portal.example/").unwrap();
        let flags = |js: &str| {
            let mut routes = Routes::default();
            routes.inspect(js, &page, &page);
            routes.post_search
        };
        assert!(flags("var Id=`search`;api.post(`/${Id}`,body)"));
        assert!(flags(
            r#"const n="notices";fetch(`/v2/${n}`,{method:"POST"})"#
        ));
        assert!(!flags("api.post(`/${route}`,body)"));
        assert!(!flags("var L=`login`;api.post(`/${L}`,body)"));
        assert!(!flags(
            r#"fetch("/Login/CheckSecurityCode",{method:"POST"})"#
        ));
        assert!(!flags("var Id=`search`;api.get(`/${Id}`)"));
    }

    #[test]
    fn meaningful_server_rendered_pages_are_skipped_and_bundle_count_is_bounded() {
        let page = Url::parse("https://portal.example/").unwrap();
        let tags: String = (0..7)
            .map(|n| format!(r#"<script src="/chunk-{n}.js"></script>"#))
            .collect();
        let html = format!(
            r#"<tn-root></tn-root><script src="https://other.example/x.js"></script>{tags}<script src="/main.js"></script>"#
        );
        let shell = scripts(&Html::parse_document(&html), &page).unwrap();
        assert_eq!(shell.scripts.len(), MAX_BUNDLES);
        assert_eq!(shell.scripts[0].path(), "/main.js");
        assert!(shell.scripts.iter().all(|u| u.origin() == page.origin()));
        assert!(
            scripts(
                &Html::parse_document(&(html + &"Procurement notice ".repeat(50))),
                &page
            )
            .is_none()
        );
    }
}
