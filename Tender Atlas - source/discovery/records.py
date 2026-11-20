"""Infer replayable field mappings from repeated notice links or structured JSON."""

import json
import re
from collections import defaultdict
from dataclasses import dataclass
from urllib.parse import parse_qsl, urljoin, urlsplit

from bs4 import BeautifulSoup
from discovery.dates import clean_date
from lxml import etree

from discovery.public import DiscoveryError, Page, public_url


def words(value):
    return " ".join(str(value).split())


def safe_link(base, href):
    try:
        return str(public_url(urljoin(base, href)))
    except (ValueError, TypeError):
        return ""


def signature(url):
    parts = urlsplit(url)
    path = re.sub(r"(?i)[a-f0-9-]{8,}|\d+", "#", parts.path)
    return (
        parts.netloc
        + path
        + "?"
        + ",".join(sorted(k for k, _ in parse_qsl(parts.query)))
    )


DOMAIN = re.compile(
    r"tender|procurem|contract|notice|opportunit|aanbested|appel.{0,3}offr|marché|cft|vergab|licitaci",
    re.IGNORECASE,
)
BUYER = re.compile(
    r"^(ca|buyer|authority|contracting authority|organisation|acheteur|opdrachtgever)( name)?$",
    re.IGNORECASE,
)
DATE = re.compile(r"publish|publication|publicatie|parution", re.IGNORECASE)


@dataclass
class Option:
    rule: dict
    rows: list
    score: int


def plain(node):
    return words(node.get_text(" ", strip=True)) if node else ""


def html_options(page, soup):
    groups = defaultdict(dict)
    following = [
        (link.get("title", "Public notice feed"), safe_link(page.url, link["href"]))
        for link in soup.select('link[rel~="alternate"][href]')
        if any(t in link.get("type", "") for t in ("rss", "atom", "json"))
        and safe_link(page.url, link["href"])
    ]
    next_url = None
    for a in soup.select("a[href], button[href]"):
        title = plain(a)
        href = a["href"].strip()
        if not href or href.startswith("#"):
            continue
        url = safe_link(page.url, href)
        if not url or urlsplit(url).hostname != urlsplit(page.url).hostname:
            continue
        label = (
            title + " " + a.get("aria-label", "") + " " + a.get("title", "")
        ).strip()
        if "next" in a.get("rel", []) or re.match(
            r"^(next|volgende|suivant|siguiente|weiter)\b|^next\d", label, re.IGNORECASE
        ):
            next_url = next_url or url
        if not title or url == page.url:
            continue
        listing_link = DOMAIN.search(title) or re.search(
            r"search|zoeken|recherche", title, re.IGNORECASE
        )
        if (
            listing_link or a.find_parent("nav") or a.find_parent("main")
        ) and not re.search(
            r"register|login|guide|how to|help|privacy|publish a|news",
            title,
            re.IGNORECASE,
        ):
            following.append((title[:120], url))
        if (
            len(title) < 10
            or not any(c.isalpha() for c in title)
            or a.find_parent(["nav", "footer", "header"])
        ):
            continue
        tr = a.find_parent("tr")
        card = None
        buyer = published = ""
        if tr and tr.find_parent("table"):
            labels = tr.find_parent("table").select("thead th") or tr.find_parent(
                "table"
            ).select("tr th")
            for th, td in zip(labels, tr.find_all("td", recursive=False)):
                name = plain(th)
                if BUYER.search(name):
                    buyer = plain(td)
                if DATE.search(name):
                    published = clean_date(plain(td))
        else:
            card = a.find_parent(["article", "li"]) or a.find_parent(
                class_=re.compile(r"^(search-result|notice-card|result-item)$")
            )
            if card:
                field = card.select_one(
                    '[itemprop="hiringOrganization"], [class*="buyer"], [class*="sub-header"]'
                )
                buyer = plain(field)
                for label_node in card.select("strong,dt"):
                    if DATE.search(plain(label_node)):
                        value = (
                            label_node.find_next_sibling("dd")
                            if label_node.name == "dt"
                            else label_node.next_sibling
                        )
                        published = clean_date(
                            words(
                                value.get_text(" ", strip=True)
                                if hasattr(value, "get_text")
                                else value or ""
                            )
                        )
                        break
        key = signature(url)
        groups[key][url] = {
            "raw": {"html": str(tr or card or a)},
            "locator": {"kind": "notice_link", "href": href, "url": url},
            "title": title,
            "url": url,
            "buyer": buyer,
            "published": published,
        }
    options = []
    for key, items in groups.items():
        rows = list(items.values())
        if len(rows) < 2 or len({r["title"] for r in rows}) < 2:
            continue
        path = key.partition("/")[2]
        score = (6 if DOMAIN.search(path) else 0) + (
            4 if any(r["buyer"] or r["published"] for r in rows) else 0
        )
        if DOMAIN.search(" ".join(r["title"] for r in rows[:5])):
            score += 2
        options.append(Option({"kind": "html", "pattern": key}, rows, score))
    following.sort(
        key=lambda pair: (
            not bool(
                re.search(
                    r"latest|current|open|list|zoeken|search notices|opportunities",
                    pair[0],
                    re.IGNORECASE,
                )
            ),
            len(pair[0]),
        )
    )
    return options, list(dict.fromkeys(following)), next_url


def json_options(page, data):
    options = []

    def visit(value, path):
        if isinstance(value, dict) and len(path) < 5:
            for k, v in value.items():
                visit(v, path + [k])
        if not isinstance(value, list) or not value or not isinstance(value[0], dict):
            return

        def flatten(obj, prefix=""):
            fields = {}
            for k, v in obj.items():
                name = prefix + k
                if isinstance(v, str):
                    fields[name] = v
                elif isinstance(v, dict) and name.count(".") < 2:
                    fields.update(flatten(v, name + "."))
            return fields

        records = [flatten(v) if isinstance(v, dict) else {} for v in value[:100]]
        keys = list(records[0])

        def pick(regex):
            return next((k for k in keys if re.search(regex, k, re.IGNORECASE)), "")

        title = pick(
            r"(?:^|\.)(?:title|objet|aanbestedingNaam|noticeTitle|contractTitle|tenderTitle)$"
        )
        link = pick(r"(?:^|\.)(?:url|href|webUrl|noticeUrl|url_avis|detailUrl)$")
        if not title or not link:
            return
        buyer = pick(
            r"buyer(?:\.name|Name)?$|opdrachtgeverNaam$|nomacheteur$|authorityName$"
        )
        published = pick(
            r"publicatieDatum$|published(?:At|Date)?$|publicationDate$|dateparution$|datePublished$"
        )
        rule = {
            "kind": "json",
            "path": path,
            "title": title,
            "link": link,
            "buyer": buyer,
            "published": published,
        }
        rows = []
        for index, record in enumerate(records):
            href = record.get(link, "")
            url = safe_link(page.url, href) if href else ""
            if record.get(title) and url:
                rows.append(
                    {
                        "title": record[title],
                        "raw": value[index],
                        "locator": {
                            "kind": "json_pointer",
                            "pointer": "/"
                            + "/".join(
                                str(k).replace("~", "~0").replace("/", "~1")
                                for k in [*path, index]
                            ),
                        },
                        "url": url,
                        "buyer": record.get(buyer, ""),
                        "published": clean_date(record.get(published, "")),
                    }
                )
        if rows:
            options.append(
                Option(
                    rule,
                    rows,
                    6
                    if DOMAIN.search(str(path) + page.url + " ".join(keys)) or buyer
                    else 0,
                )
            )

    visit(data, [])
    return options


def feed_options(page):
    try:
        root = etree.fromstring(
            page.body,
            etree.XMLParser(resolve_entities=False, load_dtd=False, no_network=True),
        )
    except etree.XMLSyntaxError:
        return [], [], None
    if root.getroottree().docinfo.doctype or etree.QName(root).localname not in {
        "feed",
        "rss",
    }:
        return [], [], None
    nodes = root.findall("{*}entry") or root.findall("./{*}channel/{*}item")
    rows = []
    for index, n in enumerate(nodes[:60]):
        title = words(n.findtext("{*}title") or "")
        link = next(
            (
                a.get("href") or a.text
                for a in n.findall("{*}link")
                if a.get("rel", "alternate") == "alternate"
            ),
            "",
        )
        url = safe_link(page.url, link) if link else ""
        if title and url:
            rows.append(
                {
                    "title": title,
                    "url": url,
                    "buyer": n.findtext("{*}buyer") or "",
                    "published": clean_date(
                        n.findtext("{*}published") or n.findtext("{*}pubDate") or ""
                    ),
                    "raw": etree.tostring(n, encoding="unicode"),
                    "locator": {"kind": "feed_entry", "one_based_entry": index + 1},
                }
            )
    context = (
        page.url
        + " "
        + (root.findtext("{*}title") or root.findtext("./{*}channel/{*}title") or "")
    )
    if rows and DOMAIN.search(context + " " + " ".join(r["title"] for r in rows[:5])):
        return [Option({"kind": "feed"}, rows, 6)], [], None
    return [], [], None


def inspect(page: Page):
    if "json" in page.mime or page.body.lstrip()[:1] in {b"{", b"["}:
        return json_options(page, json.loads(page.body)), [], None
    if "xml" in page.mime or re.search(rb"<(?:rss|feed)\b", page.body[:512]):
        return feed_options(page)
    if "html" not in page.mime and b"<html" not in page.body[:1000].lower():
        raise DiscoveryError("This address did not return a readable listing.")
    return html_options(page, BeautifulSoup(page.body, "lxml"))
