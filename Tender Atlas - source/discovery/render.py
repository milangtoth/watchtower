"""Observe a public JavaScript listing. All network reads use the bounded reader."""

import asyncio
import os
from urllib.parse import urlsplit

import httpx

from discovery import access
from discovery.public import DiscoveryError, Page


async def rendered(reader, page):
    loop = asyncio.get_running_loop()

    def observe():
        from playwright.sync_api import Error, sync_playwright

        documents = []
        blocked = None
        host = urlsplit(page.url).hostname
        with sync_playwright() as pw:
            use_edge = os.name == "nt" and not os.environ.get(
                "PLAYWRIGHT_BROWSERS_PATH"
            )
            browser = pw.chromium.launch(
                channel="msedge" if use_edge else None,
                proxy={"server": "http://127.0.0.1:9"},
                args=["--force-webrtc-ip-handling-policy=disable_non_proxied_udp"],
            )
            ctx = browser.new_context(service_workers="block", accept_downloads=False)
            ctx.route_web_socket("**/*", lambda ws: ws.close())
            tab = ctx.new_page()

            def route_request(route):
                nonlocal blocked
                if blocked:
                    return route.abort()
                req = route.request
                if req.resource_type == "document" and access.challenge_url(req.url):
                    try:
                        access.remember(page.url)
                    except access.CaptchaExcluded as exc:
                        blocked = exc
                        return route.abort()
                if (
                    req.method != "GET"
                    or urlsplit(req.url).hostname != host
                    or req.resource_type not in {"document", "script", "xhr", "fetch"}
                    or (req.resource_type == "document" and req.url != page.url)
                ):
                    return route.abort()
                try:
                    if req.url == page.url:
                        resource = page
                    else:
                        future = asyncio.run_coroutine_threadsafe(
                            reader.get(req.url, redirects=False), loop
                        )
                        try:
                            resource = future.result(timeout=12)
                        except TimeoutError:
                            future.cancel()
                            raise
                    if "json" in resource.mime and len(documents) < 8:
                        documents.append(resource)
                    route.fulfill(
                        status=200, content_type=resource.mime, body=resource.body
                    )
                except access.CaptchaExcluded as exc:
                    blocked = exc
                    route.abort()
                except (Error, httpx.HTTPError, DiscoveryError, OSError):
                    route.abort()

            ctx.route("**/*", route_request)
            try:
                tab.goto(page.url, wait_until="domcontentloaded", timeout=16000)
                tab.wait_for_timeout(2500)
                html = tab.content().encode()
                access.inspect(page.url, {"content-type": "text/html"}, html)
                documents.append(Page(page.url, html, "text/html", 200))
            finally:
                browser.close()
                if blocked:
                    raise blocked
        return documents

    try:
        return await asyncio.to_thread(observe)
    except access.CaptchaExcluded:
        raise
    except Exception as exc:
        raise DiscoveryError("The browser could not read this listing.") from exc
