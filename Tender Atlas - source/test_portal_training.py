from unittest.mock import patch

import indexdb as db
from discovery import ranking, service
from discovery.public import Page
from discovery.records import inspect


def test_held_out_publisher_ranking():
    cases = ranking.starter()
    for portal in ("nl", "fr", "ted"):
        training = [r for r in cases if r["portal"] != portal]
        test = [r for r in cases if r["portal"] == portal]
        model = ranking.active_weights.set(ranking.fit(training))
        try:
            assert (
                max(test, key=lambda r: ranking.score(r["url"], r["text"]))["label"]
                == 1
            )
        finally:
            ranking.active_weights.reset(model)


def test_checked_sample_survives_failed_refresh_and_imports_once(tmp_path, monkeypatch):
    monkeypatch.setattr(db, "DB", None)
    db.setup(tmp_path / "atlas.sqlite")
    service.setup()
    entry = "https://portal.example/"
    url = entry + "notices/42"
    row = {
        "title": "School heating contract",
        "url": url,
        "buyer": "City",
        "published": "2026-11-20",
    }
    recipe = {
        "rule": {"kind": "sitemap"},
        "url": entry + "sitemap.xml",
        "sitemap": entry + "sitemap.xml",
    }

    async def ok(reader, *args, **kwargs):
        reader.documents[url] = {
            "request": {"url": url, "method": "GET"},
            "response_url": url,
        }
        return recipe, {
            "rows": [row],
            "checks": ["From public record"],
            "verified": True,
            "checked": "2026-11-20T14:00:00Z",
        }

    with patch("discovery.service.read_preview", ok):
        result = service.preview(entry)
    service.label(entry, recipe["url"], 0)
    assert any(
        r["label"] == 0 and r["evidence"] == "user reviewed" for r in service.examples()
    )
    with db.conn() as c:
        c.execute("UPDATE portal_previews SET attempted=0")
    with patch("discovery.service.read_preview", side_effect=ValueError("Unavailable")):
        stale = service.preview(entry)
    assert stale["sample"] == result["sample"] and "last checked" in stale["warning"]
    service.add_to_register(entry)
    service.add_to_register(entry)
    assert db.search({"source": "portal"})["total"] == 1


def test_atom_feed_has_source_link_and_record_position():
    url = "https://buyer.example/tenders.atom"
    xml = b'<feed xmlns="http://www.w3.org/2005/Atom"><title>Tenders</title><entry><title>Parks maintenance</title><link href="/notice/1"/><published>2026-11-20</published></entry></feed>'
    options, _, _ = inspect(Page(url, xml, "application/atom+xml", 200))
    assert options[0].rows[0]["url"] == "https://buyer.example/notice/1"
    assert options[0].rows[0]["locator"]["one_based_entry"] == 1
