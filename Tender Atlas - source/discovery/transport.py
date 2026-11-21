"""Apply the shared publisher policy to each HTTP hop of a bulk download."""
import io
import zlib
from urllib.error import URLError
from urllib.parse import urlsplit
from urllib.request import HTTPHandler, HTTPSHandler, HTTPRedirectHandler, build_opener

from discovery import access


class CheckedResponse:
    """Replay the inspected prefix while leaving large publisher files streamed."""

    def __init__(self, response, prefix):
        self.response = response
        self.prefix = io.BytesIO(prefix)

    def __getattr__(self, name):
        return getattr(self.response, name)

    def read(self, size=-1):
        lead = self.prefix.read(size)
        if size >= 0 and len(lead) == size:
            return lead
        return lead + self.response.read(size - len(lead) if size >= 0 else None)

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.response.close()


class CheckedHop:
    def do_open(self, http_class, request, **kwargs):
        url = request.full_url
        with access.slot(url):
            access.inspect(url, {})
            response = super().do_open(http_class, request, **kwargs)
            try:
                status, headers = response.status, response.headers
                access.inspect(url, headers, status=status)
                first = response.peek(512)[:512].lstrip().lower()
                mime = headers.get("content-type", "").lower()
                if "html" in mime or first.startswith((b"<!doctype html", b"<html", b"<form")):
                    body = response.read()
                elif status >= 400 or "json" in mime or first.startswith(b"{"):
                    body = response.read(access.PROBE_BYTES)
                else:
                    return response
                probe = body
                if headers.get("content-encoding", "").lower() == "gzip" and body.startswith(b"\x1f\x8b"):
                    probe = zlib.decompressobj(16 + zlib.MAX_WBITS).decompress(body, access.PROBE_BYTES)
                access.inspect(url, headers, probe, status)
                return CheckedResponse(response, body)
            except BaseException:
                response.close()
                raise


class CheckedHTTP(CheckedHop, HTTPHandler):
    pass


class CheckedHTTPS(CheckedHop, HTTPSHandler):
    pass


class PublisherRedirect(HTTPRedirectHandler):
    def redirect_request(self, request, fp, code, msg, headers, newurl):
        if urlsplit(newurl).scheme not in {"http", "https"}:
            raise URLError("The publisher redirected to an unsupported address.")
        return super().redirect_request(request, fp, code, msg, headers, newurl)


def opener(tls):
    return build_opener(CheckedHTTP(), CheckedHTTPS(context=tls), PublisherRedirect())

