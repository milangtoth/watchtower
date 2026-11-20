"""Find public sitemap indexes and sample the notice pages they advertise."""

import gzip
import io
import json
import re
import zlib
from collections import deque
from urllib.parse import urljoin, urlsplit

import httpx
from bs4 import BeautifulSoup
from lxml import etree

from discovery import ranking
from discovery.public import MAX_SITEMAP_BYTES, DiscoveryError
from discovery.records import (
    BUYER,
    DATE,
    DOMAIN,
    Option,
    clean_date,
    plain,
    safe_link,
)

MAP_LIMIT = 6
PAGE_LIMIT = 10
READ_ERRORS = (ValueError, OSError, httpx.HTTPError)
NOTICE_PATH = re.compile(DOMAIN.pattern + r"|aankondiging|/avis\b", re.IGNORECASE)
DATED_LABEL = re.compile(
    r"closing|deadline|submission|sluiting|date limite|reference|cpv", re.IGNORECASE
)


def map_address(url):
    path = urlsplit(url).path.lower()
    return path.endswith((".xml", ".xml.gz")) or "sitemap" in path.rsplit("/", 1)[-1]


def parse_map(page):
    """Return the kind and its direct loc entries, ignoring extension metadata."""
    body = page.body
    if body.startswith(b"\x1f\x8b"):
        try:
            with gzip.GzipFile(fileobj=io.BytesIO(body)) as stream:
                body = stream.read(MAX_SITEMAP_BYTES + 1)
        except (OSError, EOFError, zlib.error) as exc:
            raise DiscoveryError("The compressed sitemap could not be read.") from exc
        if len(body) > MAX_SITEMAP_BYTES:
            raise DiscoveryError("The expanded sitemap exceeds the preview limit.")
    elif (
        "xml" not in page.mime
        and not map_address(page.url)
        and not re.search(rb"<(?:[\w.-]+:)?(?:sitemapindex|urlset)\b", body[:2048])
    ):
        return None
    parser = etree.XMLParser(resolve_entities=False, load_dtd=False, no_network=True)
    try:
        root = etree.fromstring(body, parser)
    except etree.XMLSyntaxError:
        return None
    if root.getroottree().docinfo.doctype:
        return None
    kind = etree.QName(root).localname
    if kind not in {"sitemapindex", "urlset"}:
        return None
    child_tag = "sitemap" if kind == "sitemapindex" else "url"
    entries = []
    for child in root.iterfind("{*}" + child_tag):
        loc = (child.findtext("{*}loc") or "").strip()
        if loc:
            entries.append((loc, child.findtext("{*}lastmod") or ""))
    # Prefer notice-specific paths and recently modified pages when available.
    entries.sort(
        key=lambda item: (
            ranking.score(item[0]) + bool(NOTICE_PATH.search(urlsplit_path(item[0]))),
            item[1],
        ),
        reverse=True,
    )
    return kind, entries


def urlsplit_path(value):
    # Ignore the hostname when ranking, even on a domain named "tenders".
    return value.split("://", 1)[-1].partition("/")[2]


async def sitemap_roots(reader, start):
    soup = BeautifulSoup(start.body, "lxml")
    roots = [
        node["href"]
        for node in soup.select("a[href], link[href]")
        if "sitemap" in node.get("rel", []) or "sitemap" in node["href"].lower()
    ]
    try:
        robots = await reader.get(urljoin(start.url, "/robots.txt"))
        roots.extend(
            re.findall(
                r"(?im)^\s*sitemap\s*:\s*([^\s#]+)",
                robots.body.decode("utf-8-sig", "replace"),
            )
        )
    except READ_ERRORS:
        pass
    roots.sort(
        key=lambda url: bool(NOTICE_PATH.search(urlsplit_path(url))), reverse=True
    )
    roots.append("/sitemap.xml")
    return list(dict.fromkeys(safe_link(start.url, url) for url in roots))


async def candidates(reader, start, direct):
    """Yield a bounded sample. Check a child index before unrelated roots."""
    roots = [start.url] if direct is not None else await sitemap_roots(reader, start)
    queue = deque((url, url) for url in roots if url)
    seen_maps, seen_pages = set(), set()
    host = urlsplit(start.url).hostname
    while queue and len(seen_maps) < MAP_LIMIT and len(seen_pages) < PAGE_LIMIT:
        root_url, url = queue.popleft()
        if url in seen_maps:
            continue
        seen_maps.add(url)
        ranking.remember(url, "Sitemap")
        try:
            page = start if url == start.url else await reader.get(url, sitemap=True)
            parsed = direct if page is start else parse_map(page)
        except READ_ERRORS:
            continue
        if parsed is None:
            continue
        kind, entries = parsed
        children = []
        for loc, _ in entries:
            dest = safe_link(page.url, loc)
            if not dest:
                continue
            if kind == "sitemapindex":
                if dest not in seen_maps and dest not in children:
                    children.append(dest)
                if len(children) >= MAP_LIMIT:
                    break
            elif (
                urlsplit(dest).hostname == host
                and dest not in seen_pages
                and not urlsplit(dest)
                .path.lower()
                .endswith((".pdf", ".zip", ".png", ".jpg"))
            ):
                seen_pages.add(dest)
                ranking.remember(dest, "Sitemap entry")
                yield root_url, page.url, dest
                if len(seen_pages) == PAGE_LIMIT:
                    return
        queue.extendleft((root_url, child) for child in reversed(children))


async def detail(reader, url):
    """Use displayed procurement fields, or the public record behind a TenderNed detail URL."""
    parts = urlsplit(url)
    match = re.fullmatch(r"/aankondigingen/overzicht/(\d+)/?", parts.path)
    if parts.hostname == "www.tenderned.nl" and match:
        notice_id = match.group(1)
        page = await reader.get(
            f"https://www.tenderned.nl/papi/tenderned-rs-tns/v2/publicaties/{notice_id}"
        )
        data = json.loads(page.body)
        if (
            isinstance(data, dict)
            and str(data.get("publicatieId")) == notice_id
            and data.get("aanbestedingNaam")
        ):
            return {
                "title": data["aanbestedingNaam"],
                "origin": page.url,
                "raw": data,
                "locator": {"kind": "json_pointer", "pointer": ""},
                "buyer": data.get("opdrachtgeverNaam") or "",
                "published": clean_date(data.get("publicatieDatum")),
                "url": url,
            }, None
        return None, None
    page = await reader.get(url)
    if "html" not in page.mime:
        return None, page
    soup = BeautifulSoup(page.body, "lxml")
    content = soup.select_one("main") or soup.body or soup
    title = plain(content.find("h1"))
    buyer = published = ""
    has_contract_field = False
    for label in content.select("dt, th, strong"):
        key = plain(label).rstrip(":").strip()
        value = (
            label.find_next_sibling(["dd", "td"])
            if label.name in {"dt", "th"}
            else label.next_sibling
        )
        value = plain(value) if hasattr(value, "get_text") else str(value or "").strip()
        if not key or not value:
            continue
        if BUYER.fullmatch(key):
            buyer = value
        elif DATE.search(key):
            published = clean_date(value)
        elif DATED_LABEL.search(key):
            has_contract_field = True
    if (
        title
        and buyer
        and (has_contract_field or (published and NOTICE_PATH.search(parts.path)))
    ):
        return {
            "title": title,
            "buyer": buyer,
            "published": published,
            "url": page.url,
            "raw": {"html": str(content)},
        }, None
    return None, page


async def from_sitemap(reader, start, direct, listing):
    rows = {}
    root = None
    async for root_url, map_url, url in candidates(reader, start, direct):
        if root is not None and root != root_url:
            break
        try:
            row, page = await detail(reader, url)
            if row:
                row["sitemap_file"] = map_url
                root = root_url
                rows[row["url"]] = row
                if len(rows) == 5:
                    break
            elif page:
                result = await listing(page)
                if result:
                    option, pagination = result
                    recipe = {
                        "rule": option.rule,
                        "url": page.url,
                        "render": False,
                        "sitemap": map_url,
                    }
                    return option, pagination, recipe
        except READ_ERRORS:
            continue
    if rows:
        option = Option({"kind": "sitemap"}, list(rows.values()), 0)
        return (
            option,
            None,
            {"rule": option.rule, "url": root, "render": False, "sitemap": root},
        )
    return None
