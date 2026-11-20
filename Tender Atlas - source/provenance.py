"""Provenance for a publisher response and an individual record within it."""
import hashlib
import json
from datetime import datetime, timezone
from urllib.parse import urlsplit


def captured(url, response, body=None, discovered_from=None):
    request = {"url": url, "method": "POST" if body is not None else "GET"}
    if body is not None:
        request["json_body"] = dict(body)
    document = {
        "evidence": "captured",
        "request": request,
        "response_url": response.geturl(),
        "observed_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
    }
    if discovered_from:
        document["discovered_from"] = discovered_from
    return document


def archive_evidence(url, digest):
    return {"evidence": "verified_from_archive", "request": {"url": url, "method": "GET"},
            "response_sha256": digest}


def file_digest(path):
    with path.open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def located(record, document, locator):
    record["document"] = document
    record["locator"] = locator
    return record


def json_location(pointer, member=None):
    location = {"kind": "json_pointer", "pointer": pointer}
    if member is not None:
        location["archive_member"] = member
    return location


def csv_location(number):
    return {"kind": "csv_data_row", "one_based_row": number}


def atom_location(number, member=None):
    location = {"kind": "xpath", "xpath": f"/atom:feed/atom:entry[{number}]",
                "namespaces": {"atom": "http://www.w3.org/2005/Atom"}}
    if member is not None:
        location["archive_member"] = member
    return location


def for_record(row):
    """Legacy records retain the original URL without invented response positions."""
    if row["document_json"]:
        result = json.loads(row["document_json"])
        result["record_locator"] = json.loads(row["locator_json"])
    else:
        result = {"evidence": "legacy_url_only", "request": {"url": row["origin"]},
                  "record_locator": None}
    parsed = urlsplit(result["request"]["url"])
    result["request"].update(host=parsed.netloc, path=parsed.path, query=parsed.query)
    result["record_url"] = row["url"]
    result["publisher_reference"] = row["ref"]
    result["source"] = row["source"]
    return result
