import asyncio
import json
from time import time

import httpx
import pytest

import indexdb
from discovery import service, ted
from discovery.public import DiscoveryError, Reader
from html import escape

from discovery.public import Page
from discovery.records import inspect
from discovery.preview import read_preview

BASE = "https://procurement.example"


def test_ted_home_bypasses_cached_calendar_and_uses_latest_issue(tmp_path, monkeypatch):
    entry = "https://ted.europa.eu/en/"
    monkeypatch.setattr(indexdb, "DB", None)
    indexdb.setup(tmp_path / "atlas.sqlite")
    service.setup()
    with indexdb.conn() as c:
        c.execute("INSERT INTO portal_previews VALUES (?,?,?,?,?,?)", (
            entry, '{"rule":{"kind":"html"}}', '{"rows":[{"title":"S 212 / 2026"}]}',
            "[]", time(), "",
        ))
    def search(request):
        assert request.method == "POST" and str(request.url) == ted.TED
        payload = json.loads(request.content)
        assert payload["scope"] == "LATEST" and payload["limit"] == 5 and payload["page"] == 1
        assert payload["query"].endswith("SORT BY publication-number DESC")
        return httpx.Response(200, json={"totalNoticeCount": 100, "notices": [
            {"publication-number": f"{n}-2026", "publication-date": "2026-11-20+01:00",
             "notice-title": {"eng": f"Notice {n}"}, "buyer-name": {"eng": ["Council"]}}
            for n in range(15, 10, -1)
        ]})
    client = httpx.AsyncClient(transport=httpx.MockTransport(search))
    monkeypatch.setattr(httpx, "AsyncClient", lambda **kwargs: client)
    result = service.preview(entry)
    rows = result["sample"]["rows"]
    assert not result["warning"] and [r["publication_number"] for r in rows] == [f"{n}-2026" for n in range(15,10,-1)]
    assert rows[-1]["url"].endswith("/11-2026") and rows[-1]["locator"]["pointer"] == "/notices/4"
    assert rows[0]["document"]["request"]["json_body"]["scope"] == "LATEST"


def test_ted_does_not_accept_partial_search():
    async def run():
        async with httpx.AsyncClient(transport=httpx.MockTransport(
            lambda req: httpx.Response(200,json={"timedOut":True,"notices":[],"totalNoticeCount":100})
        )) as client:
            with pytest.raises(DiscoveryError):
                await ted.latest(Reader(client),"en")
    asyncio.run(run())
    assert ted.portal_language("https://ted.europa.eu/nl") == "nl"
    assert ted.portal_language("https://ted.europa.eu/en/notice/-/detail/1-2026") is None


def html(url, markup):
    return Page(url, markup.encode(), "text/html", 200)


def test_portal_name_does_not_make_news_items_into_notices():
    home = html(
        BASE,
        '<a href="/article/12">Latest changes to procurement</a>'
        '<a href="/article/13">Procurement platform updates</a>',
    )
    candidates, _, _ = inspect(home)
    assert all(group.score < 4 for group in candidates)


def test_repeated_page_is_reported_and_prevents_recipe_verification(monkeypatch):

    names = ["Bridge inspection contract", "School roof replacement"]
    links = "".join(
        f'<a href="/notice/{i}">{escape(name)}</a>' for i, name in enumerate(names)
    )
    pages = {
        BASE: html(BASE, links + '<a rel="next" href="/list?page=2">Next</a>'),
        BASE + "/list?page=2": html(BASE + "/list?page=2", links),
    }
    for i, name in enumerate(names):
        url = BASE + f"/notice/{i}"
        pages[url] = html(url, "<h1>" + escape(name) + "</h1>")

    class Reader:
        client = None

        async def get(self, url):
            return pages[url]

    _, report = asyncio.run(read_preview(Reader(), BASE))
    assert not report["verified"]
    assert "2 of 2 sampled titles" in report["checks"][0]
    assert "Could not confirm" in report["checks"][-1]
