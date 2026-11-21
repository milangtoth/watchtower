"""Forward Atlas portal operations to the Watchtower worker on the local network."""
import os
import re
import shutil
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


def handles(method, path):
    """Only expose the worker routes used by the portal screen."""
    if path == "/settings/auto-refresh":
        return method in {"GET", "POST"}
    routes = {
        "GET": r"/(?:portals(?:/[\w-]+(?:/(?:runs|tenders))?)?|files/[a-f0-9]{64})",
        "POST": r"/portals(?:/[\w-]+/refresh)?",
        "DELETE": r"/portals/[\w-]+",
    }
    return bool(re.fullmatch(routes[method], path))


def forward(handler, body=None):
    # Use an operator-configured backend. A portal URL is always payload, never the proxy target.
    base = os.environ.get("WATCHTOWER_URL", "http://127.0.0.1:8080").rstrip("/")
    request = Request(base + handler.path, method=handler.command, data=body,
                      headers={"Content-Type": "application/json"})
    try:
        response = urlopen(request, timeout=30)
    except HTTPError as error:
        response = error
    except (URLError, TimeoutError):
        return handler.answer({"error": "Watchtower is unavailable. Check the worker address and service."}, 503)

    with response:
        handler.headers_for(response.headers.get_content_type(), response.status)
        if response.status != 204:
            shutil.copyfileobj(response, handler.wfile)
