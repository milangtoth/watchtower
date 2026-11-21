import asyncio
import json
import socket
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import httpx
import pytest

import feeds
import indexdb
from discovery import access, service, ted
from discovery.public import Reader


@pytest.fixture
def library(tmp_path, monkeypatch):
    indexdb.setup(tmp_path / "atlas.sqlite")
    service.setup()
    monkeypatch.setattr(access, "_ready", 0.0)
    yield indexdb.DB


def test_rate_across_redirect_retry_and_parallel_discovery(library, monkeypatch):
    events = []
    ready = threading.Event()
    payload = b'{"ok":true}'

    class Publisher(BaseHTTPRequestHandler):
        def do_GET(self):
            events.append((self.path, time.monotonic()))
            ready.set()
            status = 302 if self.path == "/start" else 503 if sum(p == "/file" for p, _ in events) == 1 else 200
            self.send_response(status)
            if status == 302:
                self.send_header("Location", "/file")
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(payload)

        def log_message(self, *args):
            pass

    app = ThreadingHTTPServer(("127.0.0.1", 0), Publisher)
    local = f"http://127.0.0.1:{app.server_port}"
    threading.Thread(target=app.serve_forever, daemon=True).start()

    def response(request):
        events.append(("TED", time.monotonic()))
        return httpx.Response(200, json={"totalNoticeCount": 1, "notices": [
            {"publication-number": "1-2026", "notice-title": "Paving", "publication-date": "2026-11-20"}
        ]})

    async def run():
        fetch = asyncio.create_task(asyncio.to_thread(feeds.fetch, local + "/start"))
        assert await asyncio.to_thread(ready.wait, 5)
        async with httpx.AsyncClient(transport=httpx.MockTransport(response)) as client:
            await ted.latest(Reader(client), "en")
        assert await fetch == payload

    try:
        asyncio.run(run())
    finally:
        app.shutdown()
        app.server_close()
    assert sorted(p for p, _ in events) == ["/file", "/file", "/start", "/start", "TED"]
    assert all(b[1] - a[1] >= 1.0 for a,b in zip(events, events[1:]))


def test_challenge_stops_discovery_and_disables_previous_sample(library, monkeypatch):
    entry = "https://buyers.example/"
    data_url = "https://data.example/notices"
    saved = json.dumps({"url": data_url, "rule": {"kind": "html"}})
    with indexdb.conn() as db:
        db.execute("INSERT INTO portal_previews VALUES(?,?,?,?,?,?)",
                   (entry, saved, '{"rows":[]}', "[]", 0, ""))
    hits = []

    async def lookup(*args, **kwargs):
        return [(socket.AF_INET, socket.SOCK_STREAM, 0, "", ("93.184.216.34", 443))]

    def response(request):
        hits.append(str(request.url))
        return httpx.Response(403, text='<form><div class="h-captcha" data-sitekey="public"></div></form>',
                              headers={"content-type": "text/html"})

    async def read(reader, *args, **kwargs):
        await reader.get(data_url)

    client = httpx.AsyncClient(transport=httpx.MockTransport(response))
    monkeypatch.setattr(httpx, "AsyncClient", lambda **kwargs: client)
    monkeypatch.setattr(asyncio.BaseEventLoop, "getaddrinfo", lookup)
    monkeypatch.setattr(service, "read_preview", read)
    assert service.preview(entry)["excluded"]
    access.configure(library)
    assert access.excluded(data_url) and entry not in service.saved()
    assert service.preview(entry, refresh=True)["sample"] is None
    assert len(hits) == 1
    with pytest.raises(ValueError, match="CAPTCHA"):
        service.add_to_register(entry)


def test_bulk_challenge_header_excludes_source_without_retry(library):
    hits = []

    class Publisher(BaseHTTPRequestHandler):
        def do_GET(self):
            hits.append(self.path)
            self.send_response(503)
            self.send_header("cf-mitigated", "challenge")
            self.end_headers()

        def log_message(self, *args):
            pass

    app = ThreadingHTTPServer(("127.0.0.1", 0), Publisher)
    threading.Thread(target=app.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{app.server_port}/export"
    try:
        with pytest.raises(access.CaptchaExcluded), access.owner("source:demo"):
            feeds.fetch(url)
        access.configure(library)
        assert access.excluded("source:demo")
        with pytest.raises(access.CaptchaExcluded):
            feeds.fetch(url)
        assert hits == ["/export"]
    finally:
        app.shutdown()
        app.server_close()


def test_notice_and_dormant_library_do_not_exclude_publisher(library):
    url = "https://buy.example/notices"
    access.inspect(url, {"content-type": "text/html"}, b'<script src="/recaptcha/api.js"></script><h1>CAPTCHA software procurement</h1>')
    access.inspect(url, {"content-type": "application/json"}, b'{"notices":[{"title":"CAPTCHA required for login"}]}')
    assert not access.excluded(url)
    with pytest.raises(access.CaptchaExcluded):
        access.inspect(url, {"location": "/captcha"}, status=302)

