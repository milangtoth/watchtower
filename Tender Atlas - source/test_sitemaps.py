import asyncio
import gzip

from discovery.public import DiscoveryError, Page
from discovery.preview import read_preview

SITE = "https://example.gov"
HOME = SITE + "/"
INDEX = SITE + "/maps/index.xml"
CHILD = SITE + "/maps/notices.xml.gz"


class FixtureReader:
    client = None

    def __init__(self, responses):
        self.responses = responses
        self.fetched = []

    async def get(self, url, **kwargs):
        self.fetched.append(url)
        if url not in self.responses:
            raise DiscoveryError("No page at this address")
        return self.responses[url]


def page(url, text, mime="text/html"):
    return Page(url, text.encode(), mime, 200)


def sitemap(kind, urls):
    child = "sitemap" if kind == "sitemapindex" else "url"
    entries = "".join(f"<{child}><loc>{url}</loc></{child}>" for url in urls)
    return f'<{kind} xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{entries}</{kind}>'


def test_advertised_index_gzip_and_replay(monkeypatch):
    notices = [SITE + f"/notices/{i}" for i in range(6)]

    def compressed(urls):
        return Page(
            CHILD,
            gzip.compress(sitemap("urlset", urls).encode()),
            "application/gzip",
            200,
        )

    reader = FixtureReader(
        {
            HOME: page(HOME, "<h1>Home</h1>"),
            SITE + "/robots.txt": page(
                SITE + "/robots.txt",
                "User-agent: *\nsItEmAp: " + INDEX + " # public",
                "text/plain",
            ),
            INDEX: page(
                INDEX, sitemap("sitemapindex", [INDEX, CHILD]), "application/xml"
            ),
            CHILD: compressed([notices[0], *notices]),
        }
    )
    for i, url in enumerate(notices):
        reader.responses[url] = page(
            url,
            f"<h1>Contract {i}</h1><dl><dt>Buyer</dt><dd>City</dd><dt>Deadline</dt><dd>2027-02-01</dd></dl>",
        )
    recipe, sample = asyncio.run(read_preview(reader, HOME))
    assert recipe["url"] == INDEX
    assert [r["url"] for r in sample["rows"]] == notices[:5]
    assert reader.fetched.count(INDEX) == 1
    assert notices[-1] not in reader.fetched
    assert sample["rows"][0]["published"] == ""
    reader.responses[CHILD] = compressed(notices[3:])
    reader.fetched.clear()
    _, sample = asyncio.run(read_preview(reader, HOME, recipe))
    assert reader.fetched[0] == INDEX
    assert HOME not in reader.fetched
    assert [r["url"] for r in sample["rows"]] == notices[3:]


def test_conventional_sitemap_finds_listing_after_unreadable_map(monkeypatch):
    listing = SITE + "/open-tenders"
    map_url = SITE + "/sitemap.xml"
    urls = [SITE + "/notice/1", SITE + "/notice/2"]
    titles = ["Roofing works at city hall", "Supply of library books"]
    reader = FixtureReader(
        {
            HOME: page(HOME, '<link rel="sitemap" href="/broken.xml">'),
            SITE + "/broken.xml": page(SITE + "/broken.xml", "<invalid", "text/xml"),
            map_url: page(map_url, sitemap("urlset", [listing]), "application/xml"),
            listing: page(
                listing,
                "".join(
                    f'<a href="{url}">{title}</a>' for url, title in zip(urls, titles)
                ),
            ),
            **{url: page(url, f"<h1>{title}</h1>") for url, title in zip(urls, titles)},
        }
    )
    recipe, sample = asyncio.run(read_preview(reader, HOME))
    assert recipe["url"] == listing
    assert sample["verified"] and sample["sitemap"] == map_url
    assert [r["url"] for r in sample["rows"]] == urls
