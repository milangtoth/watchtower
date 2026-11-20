"""Bounded, read-only requests to public HTTP(S) sites."""

import asyncio
import ipaddress
import socket
from dataclasses import dataclass
from time import monotonic
from urllib.parse import urljoin

import httpx


class DiscoveryError(ValueError):
    pass


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


_last = {}
_pace = asyncio.Lock()


class Reader:
    def __init__(self, client):
        self.client = client
        self.requests = 0

    async def get(self, value: str, *, redirects=True, limit=8_000_000) -> Page:
        url = public_url(value)
        for _ in range(5):
            self.requests += 1
            if self.requests > 100000:
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
                "user-agent": "Watchtower/0.1",
                "accept-encoding": "identity",
            }
            while True:
                async with _pace:
                    wait = _last.get(url.host, 0) + 1 - monotonic()
                    if wait <= 0:
                        _last[url.host] = monotonic()
                        break
                await asyncio.sleep(wait)
            async with self.client.stream(
                "GET",
                url.copy_with(host=str(ip)),
                headers=headers,
                extensions={"sni_hostname": url.host},
            ) as response:
                if response.is_redirect:
                    if not redirects:
                        raise DiscoveryError("Browser asset redirected.")
                    url = public_url(urljoin(str(url), response.headers["location"]))
                    continue
                if response.status_code in (401, 403):
                    raise DiscoveryError("Portal requires login or denies public access")
                response.raise_for_status()
                body = bytearray()
                async for chunk in response.aiter_bytes(chunk_size=65536):
                    body.extend(chunk)
                    if len(body) > limit:
                        raise OverflowError("Download exceeds size limit")
                return Page(
                    str(url),
                    bytes(body),
                    response.headers.get("content-type", ""),
                    response.status_code,
                )
        raise DiscoveryError("The website redirected too many times.")
