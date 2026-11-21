"""Exercise a real-model attach and repair against the switchable local portal."""
import argparse
import hashlib
import json
import time
from pathlib import Path

from check_contract import payload, request, verify_published


def wait_for(base, path, deadline, predicate):
    while time.monotonic() < deadline:
        portal = payload(base, path)
        if predicate(portal):
            return portal
        assert portal["state"] != "failed", portal
        time.sleep(0.5)
    raise AssertionError(f"timed out waiting for {path}")


def snapshot(base, path, directory, label):
    data = {
        "portal": payload(base, path),
        "tenders": payload(base, path + "/tenders"),
        "runs": payload(base, path + "/runs"),
    }
    if directory:
        (directory / f"{label}.json").write_text(json.dumps(data, indent=2) + "\n")
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", default="http://127.0.0.1:8080")
    parser.add_argument("--portal", default="http://host.docker.internal:8091/")
    parser.add_argument("--control", default="http://127.0.0.1:8091")
    parser.add_argument("--record-dir", type=Path)
    args = parser.parse_args()
    base = args.base.rstrip("/")
    directory = args.record_dir
    if directory:
        directory.mkdir(parents=True, exist_ok=True)
    assert all(item["url"] != args.portal for item in payload(base, "/portals")), "requires a fresh fixture registration"
    portal_id = payload(base, "/portals", method="POST", status=201, value={"url": args.portal})["id"]
    path = f"/portals/{portal_id}"
    print(f"Registered mock {portal_id}", flush=True)
    snapshot(base, path, directory, "01-registered")
    wait_for(base, path, time.monotonic() + 200, lambda row: row["state"] == "serving")
    before = snapshot(base, path, directory, "02-serving")
    assert len(before["tenders"]) == 2, before
    old_file = next(doc for row in before["tenders"] for doc in row["documents"])
    old_bytes = request(base, old_file["path"])
    assert hashlib.sha256(old_bytes).hexdigest() == old_file["sha256"]
    print("Initial attach published two tenders and a verified old file", flush=True)

    request(args.control.rstrip("/"), "/__fixture/change-layout", status=204, method="POST")
    refresh = payload(base, path + "/refresh", method="POST", status=202)
    deadline = time.monotonic() + 260
    wait_for(base, path, deadline, lambda row: row["state"] == "healing")
    during = snapshot(base, path, directory, "03-healing")
    assert during["portal"]["state"] == "healing", during
    assert during["tenders"] == before["tenders"]
    assert request(base, old_file["path"]) == old_bytes
    print("Healing preserved the previous snapshot and file bytes", flush=True)

    wait_for(base, path, deadline, lambda row: row["state"] == "serving")
    after = snapshot(base, path, directory, "04-repaired")
    assert {row["id"] for row in after["tenders"]} == {row["id"] for row in before["tenders"]}
    assert any(row["title"] == "Park lighting — amended specification" for row in after["tenders"]), after
    assert any(row["id"] == refresh["runId"] and row["result"] == "failed" for row in after["runs"])
    assert any(row["kind"] == "repair" and row["result"] == "ok" for row in after["runs"])
    new_file = next(doc for row in after["tenders"] for doc in row["documents"]
                    if doc["fileName"] == "PARK-01-v2.txt")
    assert new_file["sha256"] != old_file["sha256"]
    assert hashlib.sha256(request(base, new_file["path"])).hexdigest() == new_file["sha256"]
    verify_published(base, portal_id)
    print(f"Live repair verified for {portal_id}")


if __name__ == "__main__":
    main()
