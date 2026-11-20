"""Bounded, read-only requests to public HTTP(S) sites."""

import asyncio
import hashlib
import ipaddress
import socket
from dataclasses import dataclass
from discovery import access
from urllib.parse import urljoin

import httpx


class DiscoveryError(ValueError):
    pass


MAX_SITEMAP_BYTES = 16 * 1024 * 1024


def public_url(value: str) -> httpx.URL:
    try:
        url = httpx.URL(value.strip()).copy_with(fragment=None)
        valid = (
            url.scheme in {"https", "http"}
            and url.host
            and not url.userinfo
            and url.port in {None, 80, 443}
            and "%" not in url.host
        )
    except (ValueError, httpx.InvalidURL):
        valid = False
    if not valid:
        raise DiscoveryError(
            "Enter a public website address starting with https:// or http://."
        )
    return url


@dataclass
class Page:
    url: str
    body: bytes
    mime: str
    status: int



class Reader:
    def __init__(self, client):
        self.client = client
        self.requests = 0
        self.documents = {}

    async def get(self, value: str, *, redirects=True, sitemap=False) -> Page:
        with access.owner(value):
            return await self._read(value, redirects, sitemap)

    async def _read(self, value, redirects, sitemap):
        url = public_url(value)
        for _ in range(5):
            self.requests += 1
            if self.requests > 36:
                raise DiscoveryError("The preview reached its request limit.")
            addresses = await asyncio.get_running_loop().getaddrinfo(
                url.host,
                url.port or (443 if url.scheme == "https" else 80),
                type=socket.SOCK_STREAM,
            )
            ips = [ipaddress.ip_address(info[4][0]) for info in addresses]
            if not ips or any(
                not ip.is_global
                or ip.is_multicast
                or ip.is_reserved
                or getattr(ip, "ipv4_mapped", None)
                for ip in ips
            ):
                raise DiscoveryError("Only public internet addresses can be previewed.")
            ip = next((ip for ip in ips if ip.version == 4), ips[0])
            # Pin the checked address while preserving the original TLS and HTTP hostname.
            headers = {
                "host": url.netloc.decode(),
                "user-agent": "TenderAtlas/2.0",
                "accept-encoding": "identity",
            }
            async with access.async_slot(str(url)), self.client.stream(
                "GET",
                url.copy_with(host=str(ip)),
                headers=headers,
                extensions={"sni_hostname": url.host},
            ) as response:
                access.inspect(str(url), response.headers, status=response.status_code)
                if response.is_redirect:
                    if not redirects:
                        raise DiscoveryError("Browser asset redirected.")
                    url = public_url(urljoin(str(url), response.headers["location"]))
                    continue
                mime = response.headers.get("content-type", "")
                limit = (
                    MAX_SITEMAP_BYTES
                    if sitemap
                    or "xml" in mime
                    or url.path.lower().endswith((".xml", ".xml.gz"))
                    else 2_500_000
                )
                body = bytearray()
                async for chunk in response.aiter_bytes(chunk_size=65536):
                    body.extend(chunk)
                    if response.is_error and len(body) >= access.PROBE_BYTES:
                        break
                    if len(body) > limit:
                        access.inspect(str(url), response.headers, bytes(body), response.status_code)
                        raise DiscoveryError("This page is too large to preview.")
                access.inspect(str(url), response.headers, bytes(body), response.status_code)
                response.raise_for_status()
                self.documents[str(url)] = {
                    "request": {"method": "GET", "url": value},
                    "response_url": str(url),
                    "response_sha256": hashlib.sha256(body).hexdigest(),
                }
                return Page(
                    str(url),
                    bytes(body),
                    response.headers.get("content-type", ""),
                    response.status_code,
                )
        raise DiscoveryError("The website redirected too many times.")
