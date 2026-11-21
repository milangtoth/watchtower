"""Attach every portal in one split of testset/portals.yaml to a running Watchtower and report the results.

Usage:
    python scripts/live_run.py --db DATA_DIR/watchtower.sqlite3 [--base http://localhost:8080]
                               [--split development] [--timeout-min 60] [--out live-run.json]

Registers each portal, polls until none is `integrating` (or the timeout), then prints a
Markdown table of state, reason, tenders, documents, missing IDs and model cost per portal.
"""

import argparse
import json
import re
import sqlite3
import sys
import time
import urllib.request

# USD per 1M tokens: (input, cached input, output). Unknown models fall back to gpt-5.4.
PRICES = {
    "gpt-5.4": (2.50, 0.25, 15.00),
    "gpt-5.4-mini": (0.75, 0.075, 4.50),
}


def portals(path, split):
    text = open(path, encoding="utf-8").read()
    found = []
    for block in re.split(r"\n  - name: ", text)[1:]:
        name = block.splitlines()[0].strip()
        url = re.search(r"^\s+url: (\S+)", block, re.M)
        block_split = re.search(r"^\s+split: (\S+)", block, re.M)
        if url and block_split and block_split.group(1) == split:
            found.append((name, url.group(1)))
    return found


def call(base, method, path, body=None):
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(base + path, data=data, method=method,
                                     headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=30) as response:
        raw = response.read()
        return json.loads(raw) if raw else None


def cost(db, portal_id):
    if not db:
        return None, 0
    with sqlite3.connect(db) as conn:
        rows = conn.execute(
            "SELECT model, SUM(input_tokens - cached_input_tokens), SUM(cached_input_tokens),"
            " SUM(output_tokens), COUNT(*) FROM model_usage WHERE portal_id = ? GROUP BY model",
            (portal_id,)).fetchall()
    total, calls = 0.0, 0
    for model, uncached, cached, output, count in rows:
        rate = next((p for m, p in sorted(PRICES.items(), key=lambda x: -len(x[0]))
                     if model.startswith(m)), PRICES["gpt-5.4"])
        total += (uncached * rate[0] + cached * rate[1] + output * rate[2]) / 1e6
        calls += count
    return total, calls


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://localhost:8080")
    parser.add_argument("--portals", default="testset/portals.yaml")
    parser.add_argument("--split", default="development")
    parser.add_argument("--db")
    parser.add_argument("--timeout-min", type=float, default=60)
    parser.add_argument("--out", default="live-run.json")
    args = parser.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")

    registered = []
    for name, url in portals(args.portals, args.split):
        portal_id = call(args.base, "POST", "/portals", {"url": url})["id"]
        registered.append({"name": name, "url": url, "id": portal_id})
        print(f"registered {name}: {portal_id}", flush=True)

    deadline = time.time() + args.timeout_min * 60
    while True:
        for entry in registered:
            entry.update(call(args.base, "GET", f"/portals/{entry['id']}"))
        busy = [e["name"] for e in registered if e.get("state") == "integrating"]
        done = len(registered) - len(busy)
        print(f"{time.strftime('%H:%M:%S')} {done}/{len(registered)} done", flush=True)
        if not busy or time.time() > deadline:
            break
        time.sleep(30)

    for entry in registered:
        tenders = call(args.base, "GET", f"/portals/{entry['id']}/tenders")
        tenders = tenders.get("tenders", tenders) if isinstance(tenders, dict) else tenders
        entry["tenders"] = len(tenders)
        entry["open"] = sum(1 for t in tenders if t.get("status") == "open")
        entry["withDocuments"] = sum(1 for t in tenders if t.get("documents"))
        entry["documents"] = sum(len(t.get("documents") or []) for t in tenders)
        entry["nullIds"] = sum(1 for t in tenders if t.get("id") is None)
        entry["cost"], entry["calls"] = cost(args.db, entry["id"])

    json.dump(registered, open(args.out, "w", encoding="utf-8"), indent=2)
    print("\n| Portal | State | Tenders (open / with docs) | Docs | Null IDs | Model calls | Cost | Reason |")
    print("|---|---|---|---:|---:|---:|---:|---|")
    for e in registered:
        money = "" if e["cost"] is None else f"${e['cost']:.3f}"
        reason = (e.get("reason") or "").replace("|", "/").replace("\n", " ")[:160]
        print(f"| {e['name']} | {e.get('state')} | {e['tenders']} ({e['open']} / {e['withDocuments']})"
              f" | {e['documents']} | {e['nullIds']} | {e['calls']} | {money} | {reason} |")
    serving = sum(1 for e in registered if e.get("state") == "serving")
    total = sum(e["cost"] or 0 for e in registered)
    print(f"\n{serving}/{len(registered)} serving, total model cost ${total:.2f}")


if __name__ == "__main__":
    sys.exit(main())
