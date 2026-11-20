"""Discover a public XML source from one landing-page response."""
from urllib.parse import urljoin, urlsplit
from xml.etree import ElementTree as ET

from bs4 import BeautifulSoup

from app.recipes.llm import choose
from app.recipes.html import learn_html
from app.recipes.json_listing import learn_json


def xml(body):
    if b'<!doctype' in body.lower() or b'<!entity' in body.lower():
        raise ValueError('XML declarations with entities or DTDs are unsupported')
    return ET.fromstring(body)


def name(node):
    return node.tag.rsplit('}', 1)[-1]


async def discover(reader, url):
    page = await reader.get(url, redirects=False)
    if page.body.lstrip().startswith((b"{", b"[")):
        return await learn_json(page)
    try:
        if name(xml(page.body)) in {'rss', 'feed', 'urlset', 'sitemapindex'}:
            return page.url, page
    except (ET.ParseError, ValueError):
        pass
    soup = BeautifulSoup(page.body, 'html.parser')
    candidates = []
    for link in soup.select('link[href], a[href]'):
        target = urljoin(page.url, link['href'])
        rel = link.get('rel', [])
        mime = link.get('type', '')
        path = urlsplit(target).path.lower()
        if mime in ('application/rss+xml', 'application/atom+xml'):
            candidates.append((0, target, link.get_text(strip=True)[:100] or mime))
        elif 'sitemap' in rel or path.endswith('.xml'):
            candidates.append((1, target, link.get_text(strip=True)[:100] or 'XML link'))
    if not candidates:
        return await learn_html(reader, page)
    return await choose(sorted(set(candidates))), None
