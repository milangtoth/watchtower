"""Discover, check, and replay a public notice listing."""

import logging
import re
from datetime import UTC, date, datetime
from urllib.parse import urlsplit

import httpx
from bs4 import BeautifulSoup

from discovery import ranking, ted
from discovery.public import DiscoveryError
from discovery.records import inspect, plain
from discovery.sitemaps import from_sitemap, parse_map

log = logging.getLogger(__name__)
READ_ERRORS = (ValueError, httpx.HTTPError, OSError, UnicodeError)


def select_option(options):
    ranked = sorted(
        options,
        key=lambda o: (
            o.score >= 4,
            ranking.score(o.rows[0]["url"], o.rows[0]["title"]) + o.score / 12,
        ),
        reverse=True,
    )
    return ranked[0] if ranked and ranked[0].score >= 4 else None


async def discover(reader, entry, *, prefer_sitemap=False):
    start = await reader.get(entry)
    direct = parse_map(start)

    async def listing(page):
        options, _, pagination = inspect(page)
        option = select_option(options)
        return (option, pagination) if option else None

    if direct is not None or prefer_sitemap:
        result = await from_sitemap(reader, start, direct, listing)
        if result:
            return result
        if direct is not None:
            raise DiscoveryError(
                "The sitemap was readable, but its sampled pages yielded no identifiable procurement notices."
            )

    def discovered(option, pagination, page, render=False):
        return (
            option,
            pagination,
            {"rule": option.rule, "url": page.url, "render": render},
        )

    options, links, pagination = inspect(start)
    chosen = select_option(options)
    if chosen:
        return discovered(chosen, pagination, start)
    pages = [start]
    for _, url in ranking.order(links)[:4]:
        try:
            page = await reader.get(url)
            options, _, pagination = inspect(page)
        except READ_ERRORS:
            continue
        pages.append(page)
        chosen = select_option(options)
        if chosen:
            return discovered(chosen, pagination, page)
    if not prefer_sitemap:
        result = await from_sitemap(reader, start, None, listing)
        if result:
            return result
    # Use a browser only for listings without accessible notice markup.
    from discovery.render import rendered

    for page in [pages[1], start] if len(pages) > 1 else [start]:
        try:
            documents = await rendered(reader, page)
        except DiscoveryError:
            log.info("Browser discovery unavailable", exc_info=True)
            continue
        for doc in documents:
            options, _, pagination = inspect(doc)
            chosen = select_option(options)
            if chosen:
                return discovered(chosen, pagination, doc, doc.mime == "text/html")
    raise DiscoveryError(
        "No recognizable procurement listing was found. Try the public search results or notices page."
    )


def identity(url):
    # Ignore fragments and navigation context; preserve query identifiers.
    from urllib.parse import parse_qsl

    u = urlsplit(url)
    params = tuple(
        sorted(
            (k, v)
            for k, v in parse_qsl(u.query)
            if k.lower() not in {"origin", "p", "page", "ref", "utm_source"}
        )
    )
    return u.hostname, u.path, params


def norm(text):
    return " ".join(re.findall(r"\w+", text.casefold()))


async def replay(reader, recipe):
    page = await reader.get(recipe["url"])
    if recipe["render"]:
        from discovery.render import rendered

        page = (await rendered(reader, page))[-1]
    options, _, pagination = inspect(page)
    option = next((o for o in options if o.rule == recipe["rule"]), None)
    if option is None:
        raise DiscoveryError("The saved recipe no longer matches the listing.")
    return option, pagination


async def verify(reader, option, next_page, recipe):
    unique = {}
    for row in option.rows:
        unique.setdefault(identity(row["url"]), row)
    rows = list(unique.values())[:5]
    if not rows:
        raise DiscoveryError("No notices with distinct source links were found.")
    msgs = []
    matched = 0
    buyers = dates = 0
    for row in rows[:2]:
        try:
            page = await reader.get(row["url"])
            soup = BeautifulSoup(page.body, "lxml")
            for el in soup.select("script,style,nav,footer"):
                el.decompose()
            detail = norm(plain(soup))
            if (
                identity(page.url) != identity(recipe["url"])
                and norm(row["title"])
                and norm(row["title"]) in detail
            ):
                matched += 1
                row["title_matched"] = True
                if row["buyer"] and norm(row["buyer"]) in detail:
                    buyers += 1
                if row["published"]:
                    try:
                        day = date.fromisoformat(row["published"])
                        forms = [
                            day.strftime(fmt)
                            for fmt in ("%Y-%m-%d", "%d/%m/%Y", "%d %B %Y", "%d %b %Y")
                        ]
                        dates += any(norm(form) in detail for form in forms)
                    except ValueError:
                        pass
        except READ_ERRORS:
            pass
    count = min(2, len(rows))
    msgs.append(
        f"Matched {matched} of {count} sampled titles on their linked detail pages."
    )
    msgs.append(
        f"Also found {buyers} of {count} buyer names and {dates} of {count} listed dates in the detail text."
    )
    page_ok = True
    if next_page:
        try:
            next_recipe = dict(recipe, url=next_page)
            second, _ = await replay(reader, next_recipe)
            second_ids = {identity(r["url"]) for r in second.rows}
            page_ok = bool(second_ids - unique.keys())
        except READ_ERRORS:
            page_ok = False
        msgs.append(
            "Page two contains additional notices."
            if page_ok
            else "Could not confirm new notices on page two."
        )
    else:
        msgs.append("No next-page link was available to check.")
    return {
        "rows": rows,
        "checks": msgs,
        "verified": matched == count and page_ok,
        "checked": datetime.now(UTC).isoformat(timespec="seconds"),
    }


async def read_preview(reader, entry, previous=None, *, prefer_sitemap=False):
    language = ted.portal_language(entry)
    if language:
        return await ted.latest(reader, language)

    async def sample(option, pagination, recipe):
        if recipe["rule"]["kind"] == "sitemap":
            return {
                "rows": option.rows,
                "sitemap": recipe["sitemap"],
                "checks": [
                    f"Read {len(option.rows)} distinct notice records from pages linked by the sitemap. Titles and fields come from those records."
                ],
                "verified": True,
                "checked": datetime.now(UTC).isoformat(timespec="seconds"),
            }
        result = await verify(reader, option, pagination, recipe)
        if "sitemap" in recipe:
            result["sitemap"] = recipe["sitemap"]
        return result

    if previous:
        try:
            if previous["rule"]["kind"] == "sitemap":
                option, pagination, recipe = await discover(reader, previous["url"])
            else:
                option, pagination = await replay(reader, previous)
                recipe = previous
            result = await sample(option, pagination, recipe)
            if result["verified"]:
                return recipe, result
        except READ_ERRORS:
            if previous["rule"]["kind"] == "sitemap" and previous["url"] == entry:
                raise
    option, pagination, recipe = await discover(
        reader, entry, prefer_sitemap=prefer_sitemap
    )
    result = await sample(option, pagination, recipe)
    if not result["verified"]:
        visited = ranking.observed.get() or {}
        alternatives = [
            (label, url)
            for url, label in visited.items()
            if url not in {entry, recipe["url"]}
        ]
        for _, url in ranking.order(alternatives)[:2]:
            try:
                page = await reader.get(url)
                options, _, next_page = inspect(page)
                choice = select_option(options)
                if choice:
                    alternative = {
                        "rule": choice.rule,
                        "url": page.url,
                        "render": False,
                    }
                    checked = await verify(reader, choice, next_page, alternative)
                    if checked["verified"]:
                        return alternative, checked
            except READ_ERRORS:
                continue
    return recipe, result
