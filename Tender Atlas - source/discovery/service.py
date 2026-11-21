"""Persist portal previews, checked read instructions and reviewed training examples."""

import asyncio
import json
import threading
from time import time
from urllib.parse import urlsplit

import httpx

import indexdb as db
from discovery import access, ranking, ted
from discovery.preview import read_preview
from discovery.public import Reader, public_url
from feeds import TLS

LOCK = threading.Lock()
INTERVAL = 300


def setup():
    with db.conn() as c:
        c.executescript("""
        CREATE TABLE IF NOT EXISTS portal_previews (
            entry TEXT PRIMARY KEY, recipe TEXT, sample TEXT, candidates TEXT,
            attempted REAL NOT NULL, warning TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS portal_examples (
            portal TEXT NOT NULL, url TEXT NOT NULL, text TEXT NOT NULL,
            label INTEGER NOT NULL, evidence TEXT NOT NULL, PRIMARY KEY(portal,url));
        """)


def examples():
    with db.conn() as c:
        rows = ranking.starter() + [
            dict(r)
            for r in c.execute("SELECT * FROM portal_examples ORDER BY portal,url")
        ]
    return [r for r in rows if not access.excluded("https://" + r["portal"]) and not access.excluded(r["url"])]


def saved():
    with db.conn() as c:
        return [
            r[0]
            for r in c.execute(
                "SELECT entry FROM portal_previews WHERE recipe IS NOT NULL ORDER BY entry"
            ) if not access.excluded(r[0])
        ]


def present(record):
    return {
        "entry": record["entry"],
        "sample": json.loads(record["sample"] or "null"),
        "candidates": json.loads(record["candidates"] or "[]"),
        "warning": record["warning"],
    }


def excluded_preview(entry):
    return {"entry": entry, "excluded": True, "sample": None, "candidates": [],
            "warning": "This portal is excluded because it requires a CAPTCHA."}


def preview(entry, *, refresh=False):
    entry = str(public_url(entry))
    if access.excluded(entry):
        return excluded_preview(entry)
    with db.conn() as c:
        old = c.execute(
            "SELECT * FROM portal_previews WHERE entry=?", (entry,)
        ).fetchone()
    if old and old["recipe"] and access.excluded(json.loads(old["recipe"]).get("url", entry)):
        return excluded_preview(entry)
    # Earlier builds sometimes saved issue calendar links as TED notices.
    if old and old["sample"] and ted.portal_language(entry):
        previous_rule = json.loads(old["recipe"] or "{}").get("rule")
        if previous_rule != ted.RULE:
            old = None
    if old and time() - old["attempted"] < (10 if refresh else INTERVAL):
        return present(old)
    if not LOCK.acquire(blocking=False):
        raise ValueError("Another portal is being read. Try again once it finishes.")
    try:
        recipe = json.loads(old["recipe"]) if old and old["recipe"] else None
        sample = json.loads(old["sample"]) if old and old["sample"] else None
        weights = ranking.fit(examples())
        seen = {}
        token = ranking.observed.set(seen)
        model = ranking.active_weights.set(weights)
        warning = ""

        async def read():
            async with httpx.AsyncClient(
                verify=TLS, trust_env=False, timeout=9
            ) as client:
                reader = Reader(client)
                instructions, result = await read_preview(
                    reader,
                    entry,
                    recipe,
                    prefer_sitemap=urlsplit(entry).hostname
                    in {"www.tenderned.nl", "tenderned.nl"},
                )
                for row in result["rows"]:
                    origin = row.get("origin") or (
                        row["url"]
                        if instructions["rule"]["kind"] == "sitemap"
                        else instructions["url"]
                    )
                    row["origin"] = origin
                    if origin in reader.documents:
                        row["document"] = dict(
                            reader.documents[origin],
                            evidence="captured",
                            observed_at=result["checked"],
                            discovered_from={
                                "url": row.get("sitemap_file")
                                or result.get("sitemap")
                                or entry
                            },
                        )
                return instructions, result

        try:
            with access.owner(entry):
                instructions, result = asyncio.run(asyncio.wait_for(read(), timeout=85))
            seen.setdefault(instructions["url"], "Notice source")
            if old and recipe and instructions == recipe and len(seen) == 1:
                for item in json.loads(old["candidates"] or "[]"):
                    seen.setdefault(item["url"], item["text"])
            if result["verified"]:
                recipe, sample = instructions, result
            else:
                warning = "The sample checks were inconclusive."
                if not recipe:
                    sample = result
        except access.CaptchaExcluded:
            return excluded_preview(entry)
        except (ValueError, OSError, httpx.HTTPError) as exc:
            warning = str(exc) or "The site did not respond in time."
        finally:
            ranking.observed.reset(token)
            ranking.active_weights.reset(model)
        if warning and recipe:
            warning += " Showing the last checked sample."
        candidates = [{"url": url, "text": text} for url, text in seen.items()]
        if not candidates and old:
            candidates = json.loads(old["candidates"] or "[]")
        record = dict(
            entry=entry,
            recipe=json.dumps(recipe) if recipe else None,
            sample=json.dumps(sample) if sample else None,
            candidates=json.dumps(candidates),
            attempted=time(),
            warning=warning,
        )
        with db.conn() as c:
            c.execute(
                """INSERT OR REPLACE INTO portal_previews(entry,recipe,sample,candidates,attempted,warning)
                         VALUES(:entry,:recipe,:sample,:candidates,:attempted,:warning)""",
                record,
            )
            if not warning and recipe:
                url = recipe["url"]
                c.execute(
                    "INSERT OR IGNORE INTO portal_examples VALUES(?,?,?,?,?)",
                    (
                        urlsplit(entry).hostname,
                        url,
                        seen.get(url, ""),
                        1,
                        "passed sample checks",
                    ),
                )
        return present(record)
    finally:
        LOCK.release()


def label(entry, url, value):
    if access.excluded(entry) or access.excluded(url):
        raise ValueError("This site is excluded because it requires a CAPTCHA.")
    if type(value) is not int or value not in (0, 1):
        raise ValueError("Choose a label.")
    with db.conn() as c:
        result = c.execute(
            "SELECT candidates FROM portal_previews WHERE entry=?", (entry,)
        ).fetchone()
        candidates = (
            {x["url"]: x["text"] for x in json.loads(result[0])} if result else {}
        )
        if url not in candidates:
            raise ValueError("Preview this portal again to label this source.")
        c.execute(
            "INSERT OR REPLACE INTO portal_examples VALUES(?,?,?,?,?)",
            (urlsplit(entry).hostname, url, candidates[url], value, "user reviewed"),
        )
    return {"saved": True}


def add_to_register(entry):
    with db.conn() as c:
        result = c.execute(
            "SELECT sample,recipe FROM portal_previews WHERE entry=? AND recipe IS NOT NULL",
            (entry,),
        ).fetchone()
    if access.excluded(entry) or (result and access.excluded(json.loads(result["recipe"]).get("url", entry))):
        raise ValueError("This site is excluded because it requires a CAPTCHA.")
    if not result:
        raise ValueError("A checked sample is required before adding records.")
    sample = json.loads(result[0])
    records = []
    for row in sample["rows"]:
        record = {k: "" for k in db.FIELDS}
        record.update(
            ref=row["url"],
            url=row["url"],
            title=row["title"],
            buyer=row.get("buyer", ""),
            country="Unspecified",
            stage="Notice",
            published=row.get("published", ""),
            origin=row["origin"],
            raw=row.get("raw")
            or {k: row.get(k, "") for k in ("title", "buyer", "published", "url")},
        )
        if row.get("document"):
            record["document"] = row["document"]
            record["locator"] = row.get("locator") or {
                "kind": "notice_link",
                "url": row["url"],
            }
        records.append(record)
    db.store("portal", records)
    return {"count": len(records)}
