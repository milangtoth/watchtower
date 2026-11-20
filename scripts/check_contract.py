"""Exercise Watchtower's HTTP contract against a running container."""
import argparse
import hashlib
import json
import re
import time
from datetime import date, datetime
from urllib.error import HTTPError
from urllib.parse import urlencode
from urllib.request import Request, urlopen
from uuid import uuid4


def request(base, path, status=200, method="GET", value=None):
    data = json.dumps(value).encode() if value is not None else None
    call = Request(base + path, data=data, method=method, headers={
        "Content-Type": "application/json"
    })
    try:
        response = urlopen(call, timeout=20)
    except HTTPError as error:
        response = error
    with response:
        assert response.code == status, f"{method} {path}: expected {status}, got {response.code}"
        return response.read()


def payload(base, path, **kwargs):
    return json.loads(request(base, path, **kwargs))


def timestamp(value):
    normalized = re.sub(r"(\.\d{6})\d+", r"\1", value.replace("Z", "+00:00"))
    parsed = datetime.fromisoformat(normalized)
    assert parsed.tzinfo is not None, f"missing offset: {value}"
    return parsed


def verify_published(base, portal_id):
    portal_path = f"/portals/{portal_id}"
    portal = payload(base, portal_path)
    assert {"id", "url", "state", "reason", "tenderCount", "lastSuccessAt"} <= portal.keys()
    assert portal["state"] == "serving", portal
    timestamp(portal["lastSuccessAt"])
    tenders = payload(base, portal_path + "/tenders")
    assert isinstance(tenders, list) and tenders, "empty published tender set"
    assert portal["tenderCount"] == len(tenders)
    documents = []
    for tender in tenders:
        assert {"id", "url", "title", "buyerName", "publicationDate", "submissionDeadline",
                "status", "documents", "lastChangedAt"} <= tender.keys()
        assert tender["status"] in {"open", "closed", "withdrawn"}
        if tender["publicationDate"] is not None:
            date.fromisoformat(tender["publicationDate"])
        if tender["submissionDeadline"] is not None:
            timestamp(tender["submissionDeadline"])
        for code in tender.get("cpvCodes", []):
            assert len(code) == 8 and code.isascii() and code.isdigit()
        assert isinstance(tender["documents"], list)
        documents.extend(tender["documents"])
    latest = max(tenders, key=lambda row: timestamp(row["lastChangedAt"]))["lastChangedAt"]
    assert payload(base, portal_path + "/tenders?" + urlencode({"changedSince": latest})) == []
    runs = payload(base, portal_path + "/runs")
    assert isinstance(runs, list) and runs and runs[0]["kind"] == "read"
    for run in runs:
        assert run["kind"] in {"read", "repair"}
        assert run["result"] in {None, "ok", "failed"}
        timestamp(run["startedAt"])
        if run["finishedAt"] is not None:
            timestamp(run["finishedAt"])
        assert (run["finishedAt"] is None) == (run["result"] is None)
    assert documents, "no published files to verify"
    manifest = documents[0]
    assert {"fileName", "sizeBytes", "sha256", "path"} <= manifest.keys()
    assert manifest["path"].startswith("/files/")
    binary = request(base, manifest["path"])
    assert len(binary) == manifest["sizeBytes"]
    assert hashlib.sha256(binary).hexdigest() == manifest["sha256"]
    print(f"Verified {len(tenders)} tenders, {len(documents)} file references, and {manifest['fileName']}")


def verify_missing(base, portal_id):
    path = f"/portals/{portal_id}"
    for suffix in ("", "/tenders", "/runs"):
        request(base, path + suffix, status=404)
    request(base, path + "/refresh", status=404, method="POST")
    request(base, path, status=404, method="DELETE")


def exercise(base, url, portals, timeout):
    assert all(portal["url"] != url for portal in portals), "exercise requires a fresh fixture URL"
    data = {"url": url}
    portal_id = payload(base, "/portals", status=201, method="POST", value=data)["id"]
    path = f"/portals/{portal_id}"
    try:
        again = payload(base, "/portals", status=201, method="POST", value=data)
        assert again == {"id": portal_id}
        refresh = payload(base, path + "/refresh", status=202, method="POST")
        again = payload(base, path + "/refresh", status=202, method="POST")
        assert set(refresh) == {"runId"} and refresh == again
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            portal = payload(base, path)
            if portal["state"] == "serving":
                verify_published(base, portal_id)
                break
            assert portal["state"] != "failed", portal
            time.sleep(2)
        else:
            raise AssertionError("fresh fixture did not reach serving")
    finally:
        assert request(base, path, status=204, method="DELETE") == b""
    verify_missing(base, portal_id)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("base", nargs="?", default="http://127.0.0.1:8080")
    modes = parser.add_mutually_exclusive_group(required=True)
    modes.add_argument("--portal-id", help="read-only check of a serving portal")
    modes.add_argument("--exercise", metavar="URL", help="register and remove a fresh mock portal")
    parser.add_argument("--timeout", type=int, default=180)
    args = parser.parse_args()
    base = args.base.rstrip("/")
    assert b"Watchtower" in request(base, "/")
    portals = payload(base, "/portals")
    assert isinstance(portals, list)
    verify_missing(base, str(uuid4()))
    request(base, "/files/" + "0" * 64, status=404)
    if args.exercise:
        exercise(base, args.exercise, portals, args.timeout)
    else:
        assert any(portal["id"] == args.portal_id for portal in portals)
        verify_published(base, args.portal_id)
    print("HTTP contract checks passed")


if __name__ == "__main__":
    main()
