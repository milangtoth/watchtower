"""One outbound request gate and persistent CAPTCHA exclusions for Tender Atlas."""
import asyncio
import json
import re
import sqlite3
import threading
import time
from contextlib import asynccontextmanager, contextmanager, closing
from contextvars import ContextVar
from html.parser import HTMLParser
from urllib.parse import urljoin, urlsplit

INTERVAL = 1.0
PROBE_BYTES = 512 * 1024
_owners = ContextVar("publisher_request_owners", default=())
_wire = threading.Lock()
_ready = 0.0
_state = threading.RLock()
_exclusions = set()
_database = None


class CaptchaExcluded(RuntimeError):
    """Terminal for this site. Never handle it as a retryable read error."""


def key(url):
    if url.startswith("source:"):
        return url
    return (urlsplit(url).hostname or "").lower().rstrip(".").removeprefix("www.")


def configure(path):
    global _database, _exclusions
    with _state, closing(sqlite3.connect(path)) as db, db:
        db.execute("CREATE TABLE IF NOT EXISTS captcha_exclusions (site TEXT PRIMARY KEY)")
        _exclusions = {row[0] for row in db.execute("SELECT site FROM captcha_exclusions")}
        _database = path


def excluded(url):
    host = key(url)
    with _state:
        return any(host == item or host.endswith("." + item) for item in _exclusions)


def remember(url):
    sites = {key(url), *_owners.get()} - {""}
    with _state:
        new = sites - _exclusions
        _exclusions.update(new)
        if new and _database is not None:
            with closing(sqlite3.connect(_database, timeout=45)) as db, db:
                db.executemany("INSERT OR IGNORE INTO captcha_exclusions VALUES(?)", [(s,) for s in new])
    raise CaptchaExcluded(f"Excluded because a CAPTCHA was detected on {key(url)}.")


def ensure(url):
    if excluded(url):
        remember(url)
    for owner in _owners.get():
        if excluded(owner if owner.startswith("source:") else "https://" + owner):
            raise CaptchaExcluded("This publisher is excluded because it requires a CAPTCHA.")


@contextmanager
def owner(url):
    ensure(url)
    token = _owners.set((*_owners.get(), key(url)))
    try:
        yield
    finally:
        _owners.reset(token)


@contextmanager
def slot(url):
    global _ready
    ensure(url)
    if challenge_url(url):
        remember(url)
    with _wire:
        time.sleep(max(0, _ready - time.monotonic()))
        ensure(url)
        try:
            yield
        finally:
            _ready = time.monotonic() + INTERVAL


@asynccontextmanager
async def async_slot(url):
    global _ready
    ensure(url)
    if challenge_url(url):
        remember(url)
    while not _wire.acquire(blocking=False):
        await asyncio.sleep(0.05)
    try:
        await asyncio.sleep(max(0, _ready - time.monotonic()))
        ensure(url)
        try:
            yield
        finally:
            _ready = time.monotonic() + INTERVAL
    finally:
        _wire.release()


def challenge_url(url):
    try:
        parts = urlsplit(url)
        host = (parts.hostname or "").lower()
    except ValueError:
        return False
    path = parts.path.lower()
    return (bool(re.search(r"/(?:captcha|human-verification|verify-human)(?:/|$|\.(?:html|php|aspx))", path))
            or "/cdn-cgi/challenge-platform/" in path
            or (host == "challenges.cloudflare.com" and "/turnstile/" in path and "api.js" not in path)
            or (host.endswith((".google.com", ".recaptcha.net")) and "/recaptcha/" in path and "/anchor" in path)
            or (host.endswith(".hcaptcha.com") and "hcaptcha.html" in path))


_PROMPT = re.compile(r"(?:verify|confirm|prove|checking)(?: that)? (?:you are|you're|you.re) (?:a )?human|"
                     r"(?:complete|solve|enter|pass) (?:the |this |a )?(?:security )?captcha|"
                     r"i(?: am|'m|’m) not a robot|verify (?:that )?you are not a robot", re.I)


class ChallengeMarkup(HTMLParser):
    def __init__(self):
        super().__init__()
        self.challenge = False
        self.silent = 0
        self.visible = []

    def handle_starttag(self, tag, attrs):
        a = dict(attrs)
        classes = (a.get("class") or "").lower().split()
        if tag in {"script", "style"}:
            self.silent += 1
        if (set(classes) & {"g-recaptcha","h-captcha","cf-turnstile","frc-captcha"}
                or tag in {"form","iframe","img"} and challenge_url(a.get("src") or a.get("action") or "")
                or tag == "input" and re.search("captcha", (a.get("name") or "")+" "+(a.get("id") or ""), re.I)
                or tag == "iframe" and "captcha" in (a.get("title") or "").lower()):
            self.challenge = True

    def handle_endtag(self,tag):
        if tag in {"script","style"} and self.silent:
            self.silent -= 1

    def handle_data(self,data):
        if not self.silent:
            self.visible.append(data)


def inspect(url, headers, body=b"", status=200):
    if (headers.get("cf-mitigated","").lower()=="challenge"
            or headers.get("x-amzn-waf-action","").lower() in {"captcha","challenge"}
            or challenge_url(url) or (300 <= status < 400 and
                challenge_url(urljoin(url,headers.get("location",""))))):
        remember(url)
    mime = headers.get("content-type","").lower()
    body = body.lstrip()
    if not body:
        return
    if "html" in mime or body[:100].lower().startswith((b"<!doctype html",b"<html",b"<form",b"<iframe")):
        parser = ChallengeMarkup()
        parser.feed(body.decode("utf-8","replace"))
        if parser.challenge or _PROMPT.search(" ".join(" ".join(parser.visible).split())):
            remember(url)
    elif status >= 400 and "json" not in mime and not body.startswith(b"{"):
        message = body[:8192].decode("utf-8", "replace")
        if _PROMPT.search(message) or re.search(r"captcha[_ -]?(required|challenge)", message, re.I):
            remember(url)
    elif "json" in mime or body.startswith(b"{"):
        try:
            data = json.loads(body)
        except (ValueError, UnicodeError):
            return
        if isinstance(data, dict):
            if any(data.get(k) is True for k in ("captcha_required","captchaRequired","requiresCaptcha")):
                remember(url)
            error = " ".join(str(data.get(k,"")) for k in ("error","message","code","detail"))
            if re.search(r"captcha[_ -]?(required|challenge)|requires? (?:a )?captcha",error,re.I) or _PROMPT.search(error):
                remember(url)

