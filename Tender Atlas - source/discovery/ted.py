"""Resolve TED's homepage to notices in its latest Official Journal issue."""

import hashlib
import re
from datetime import UTC, date, datetime
from urllib.parse import parse_qs, urlsplit

from feeds import TED, flat
from provenance import json_location

from discovery import access
from discovery.public import DiscoveryError

RULE = {"kind": "ted_latest_issue"}
LANGUAGES = set("bg cs da de el en es et fi fr ga hr hu it lt lv mt nl pl pt ro sk sl sv".split())


def portal_language(entry):
    url = urlsplit(entry)
    if url.hostname not in {"ted.europa.eu", "www.ted.europa.eu"}:
        return None
    parts = url.path.strip("/").split("/")
    language = parts.pop(0) if parts[0] in LANGUAGES else "en"
    if not any(parts):
        return language
    if parts == ["search", "result"] and parse_qs(url.query) == {"search-scope": ["LATEST"]}:
        return language
    return None


async def latest(reader, language):
    payload = {
        "scope": "LATEST",
        "query": "publication-number = * SORT BY publication-number DESC",
        "fields": ["publication-number", "publication-date", "notice-title", "buyer-name"],
        "page": 1,
        "limit": 5,
    }
    async with access.async_slot(TED):
        response = await reader.client.post(TED, json=payload)
        access.inspect(TED, response.headers, response.content, response.status_code)
        response.raise_for_status()
    data = response.json()
    notices = data.get("notices")
    total = data.get("totalNoticeCount")
    if (data.get("timedOut") or not isinstance(notices, list) or not isinstance(total, int)
            or total < 1 or len(notices) != min(total, 5)):
        raise DiscoveryError("TED did not return a complete latest-issue preview.")

    rows = []
    ids = []
    for index, notice in enumerate(notices):
        if not isinstance(notice, dict):
            raise DiscoveryError("TED returned an unreadable notice.")
        ref = notice.get("publication-number", "")
        published = str(notice.get("publication-date", ""))[:10]
        title = flat(notice.get("notice-title"))
        if not isinstance(ref, str) or not re.fullmatch(r"\d+-\d{4}", ref) or not title:
            raise DiscoveryError("A TED notice is missing its publication number or title.")
        number, year = map(int, ref.split("-"))
        ids.append((year, number))
        rows.append({
            "title": title, "buyer": flat(notice.get("buyer-name")),
            "published": date.fromisoformat(published).isoformat(), "publication_number": ref,
            "url": f"https://ted.europa.eu/{language}/notice/-/detail/{ref}",
            "origin": TED, "raw": notice, "locator": json_location(f"/notices/{index}"),
        })
    if ids != sorted(set(ids), reverse=True) or len({r["published"] for r in rows}) != 1:
        raise DiscoveryError("TED's response did not match the requested latest-issue order.")
    reader.documents[TED] = {
        "request": {"method": "POST", "url": TED, "json_body": payload},
        "response_url": str(response.url),
        "response_sha256": hashlib.sha256(response.content).hexdigest(),
    }
    return {"rule": RULE, "url": TED, "render": False}, {
        "rows": rows, "verified": True, "checked": datetime.now(UTC).isoformat(timespec="seconds"),
        "latest_issue": f"https://ted.europa.eu/{language}/search/result?search-scope=LATEST",
        "checks": [f"Read {len(rows)} notices from TED’s latest issue, published {rows[0]['published']}. "
                   "Notices within the issue are ordered by descending publication number."],
    }
